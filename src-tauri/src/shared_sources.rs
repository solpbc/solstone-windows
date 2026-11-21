// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use observer_model::{
    HealthDump, JournalLink, PairingPhase, ScreenshotsState, SourceKind, SourceState,
};
use pl_transport_win::client::SourceAdvert;
use pl_transport_win::shared_sources::{self as cache, Cached, Snapshot};
use pl_transport_win::{ObserverClient, TransportError};
use serde::{Deserialize, Serialize};
use tauri::{Emitter, Manager};
use tokio::sync::{watch, Notify};

use crate::app::AppState;

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
struct Entry {
    source: &'static str,
    state: &'static str,
    detail: Option<String>,
}

fn declaration(dump: &HealthDump) -> Option<Vec<Entry>> {
    if !dump.engine_ready {
        return None;
    }
    let obsidian = crate::obsidian::source_status()?;
    let screenshots = crate::screenshots::source_status()?;
    #[cfg(feature = "browser-host")]
    let browser = crate::browser::status()
        .filter(|b| !b.connected.is_empty())
        .map(|b| (b.capture, b.delivery));
    #[cfg(not(feature = "browser-host"))]
    let browser = None;
    Some(configured_sources(
        dump,
        obsidian.state,
        obsidian.vault,
        (!screenshots.waiting_for_journal).then_some(screenshots.state),
        browser,
    ))
}

fn configured_sources(
    dump: &HealthDump,
    vault_state: obsidian_source::State,
    vault: Option<String>,
    screenshots: Option<ScreenshotsState>,
    browser: Option<(&str, &str)>,
) -> Vec<Entry> {
    use obsidian_source::State;
    let mut entries = Vec::new();
    let mut add = |source, state, detail| {
        entries.push(Entry {
            source,
            state,
            detail,
        })
    };
    let state = match vault_state {
        State::On => Some("on"),
        State::Paused => Some("paused"),
        State::NeedsAttention => Some("needs_attention"),
        _ => None,
    };
    if let Some(state) = state {
        add("obsidian", state, vault);
    }
    let shots = match screenshots {
        Some(ScreenshotsState::On | ScreenshotsState::BringingIn) => Some("on"),
        Some(ScreenshotsState::Paused) => Some("paused"),
        Some(ScreenshotsState::FolderMissing) => Some("needs_attention"),
        _ => None,
    };
    if let Some(state) = shots {
        add("screenshots", state, None);
    }
    for (source, kinds) in [
        ("screen", vec![(SourceKind::Screen, dump.setup.screen)]),
        (
            "audio",
            vec![
                (SourceKind::Mic, dump.setup.mic),
                (SourceKind::SystemAudio, dump.setup.system_audio),
            ],
        ),
    ] {
        let enabled: Vec<_> = kinds
            .iter()
            .filter(|(_, on)| *on)
            .filter_map(|(kind, _)| dump.sources.iter().find(|r| r.kind == *kind))
            .collect();
        let fault = enabled
            .iter()
            .any(|r| matches!(r.state, SourceState::Faulted { .. }));
        let active = enabled.iter().any(|r| r.state == SourceState::Active);
        let configured = kinds.iter().any(|(_, on)| *on);
        let word = if fault {
            Some("needs_attention")
        } else if configured && dump.pause.is_some() {
            Some("paused")
        } else if active {
            Some("on")
        } else {
            None
        };
        if let Some(word) = word {
            add(source, word, None);
        }
    }
    let browser_state = match browser {
        Some(("intake_off", _) | (_, "failed")) => Some("needs_attention"),
        Some(("paused", _)) => Some("paused"),
        Some(("permitted", _)) => Some("on"),
        _ => None,
    };
    if let Some(state) = browser_state {
        add("browsers", state, None);
    }
    entries
}

#[derive(Clone, Default, PartialEq, Eq)]
struct Input {
    binding: String,
    online: bool,
    advert: Option<SourceAdvert>,
    local: Option<Vec<Entry>>,
    connection: u64,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum Refresh {
    Cached,
    Focus,
    Detail,
}

struct Shared {
    input: watch::Sender<Input>,
    refresh: Mutex<Refresh>,
    wake: Notify,
    view: Mutex<Option<View>>,
}

#[derive(Clone, Serialize)]
pub struct View {
    binding: String,
    devices: Vec<cache::Device>,
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

pub fn start(app: tauri::AppHandle) {
    let (input, rx) = watch::channel(Input::default());
    app.manage(Shared {
        input,
        refresh: Mutex::new(Refresh::Cached),
        wake: Notify::new(),
        view: Mutex::default(),
    });
    tauri::async_runtime::spawn(run(app, rx));
}

/// Called from the existing health subscription. Only a changed declaration,
/// pairing, connection, or advertisement wakes the network worker.
pub fn observe(app: &tauri::AppHandle, dump: &HealthDump, network: bool) {
    let state = app.state::<AppState>();
    let Ok(access) = state.credential_access.try_lock() else {
        return;
    };
    let client = access.as_ref().map(|a| a.client_slot().load());
    let pairing = &dump.sync.pairing;
    let shared = app.state::<Shared>();
    let mut input = if let Some(client) = client.filter(|c| {
        pairing.phase == PairingPhase::Paired
            && c.gate_open()
            && c.journal_identity().client_cert_sha256 == pairing.binding
    }) {
        if shared.input.borrow().binding != pairing.binding {
            watch_advert(
                app.clone(),
                pairing.binding.clone(),
                client.subscribe_source_advert(),
                shared.input.subscribe(),
            );
        }
        Input {
            connection: 0,
            binding: pairing.binding.clone(),
            online: network
                && client.refusal_stop().is_none()
                && !matches!(
                    dump.sync.journal_link,
                    Some(JournalLink::Offline | JournalLink::Connecting)
                ),
            advert: client.source_advert(),
            local: declaration(dump),
        }
    } else {
        Input::default()
    };
    let mut clear = false;
    shared.input.send_if_modified(|old| {
        input.connection = old.connection.wrapping_add(u64::from(
            input.online && (!old.online || old.binding != input.binding),
        ));
        clear = old.binding != input.binding
            || (old.advert != input.advert
                && matches!(
                    input.advert,
                    Some(SourceAdvert::Unsupported | SourceAdvert::Revoked)
                ));
        if *old == input {
            false
        } else {
            *old = input;
            true
        }
    });
    if clear {
        let _ = app.emit_to("settings", "shared-sources://changed", ());
    }
}

fn watch_advert(
    app: tauri::AppHandle,
    binding: String,
    mut reports: watch::Receiver<Option<SourceAdvert>>,
    mut input: watch::Receiver<Input>,
) {
    tauri::async_runtime::spawn(async move {
        loop {
            tokio::select! {
                change = input.changed() => {
                    if change.is_err() || input.borrow_and_update().binding != binding { break; }
                }
                change = reports.changed() => {
                    if change.is_err() { break; }
                    let advert = reports.borrow_and_update().clone();
                    let shared = app.state::<Shared>();
                    let hidden = matches!(advert, Some(SourceAdvert::Unsupported | SourceAdvert::Revoked));
                    if shared.input.send_if_modified(|old| {
                        if old.binding != binding || old.advert == advert { return false; }
                        old.advert = advert;
                        true
                    }) && hidden {
                        let _ = app.emit_to("settings", "shared-sources://changed", ());
                    }
                }
            }
        }
    });
}

#[tauri::command]
pub async fn known_sources(
    window: tauri::WebviewWindow,
    app: tauri::AppHandle,
    refresh: Refresh,
) -> Option<View> {
    crate::logs::require_settings(&window).ok()?;
    let shared = app.state::<Shared>();
    if refresh != Refresh::Cached {
        let mut pending = shared.refresh.lock().unwrap();
        *pending = (*pending).max(refresh);
        shared.wake.notify_one();
    }
    let state = app.state::<AppState>();
    let client = pl_transport_win::access::load_current_client(&state.credential_access).await?;
    let pairing = state.sync.lock().ok()?.pairing.clone();
    if pairing.phase != PairingPhase::Paired
        || !client.gate_open()
        || matches!(
            client.source_advert(),
            Some(SourceAdvert::Unsupported | SourceAdvert::Revoked)
        )
        || client.refusal_stop().is_some()
        || client.journal_identity().client_cert_sha256 != pairing.binding
    {
        return None;
    }
    let view = shared.view.lock().ok()?.clone()?;
    (view.binding == pairing.binding).then_some(view)
}

#[derive(Default)]
struct Progress {
    binding: String,
    connection: Option<u64>,
    revoked: bool,
    unsupported: bool,
    sent: Option<Vec<Entry>>,
    rejected: Option<Vec<Entry>>,
    rev: Option<String>,
    fetched_at: u64,
    last_advert: Option<SourceAdvert>,
}

impl Progress {
    fn begin_declaration(&mut self, local: &[Entry]) -> bool {
        if self.rejected.as_deref() != Some(local) {
            self.rejected = None;
        }
        if self.rejected.is_some() || self.sent.as_deref() == Some(local) {
            return false;
        }
        // The journal may apply this replacement even if we lose the reply.
        self.sent = None;
        true
    }
}

fn show(app: &tauri::AppHandle, view: Option<View>) {
    *app.state::<Shared>().view.lock().unwrap() = view;
    let _ = app.emit_to("settings", "shared-sources://changed", ());
}

fn apply_cache(app: &tauri::AppHandle, p: &mut Progress, value: Option<Cached>) {
    let mut view = None;
    p.rev = None;
    p.fetched_at = 0;
    match value {
        Some(Cached::Known(s)) => {
            p.rev = Some(s.sources_rev);
            p.fetched_at = s.fetched_at;
            view = Some(View {
                binding: p.binding.clone(),
                devices: s.devices,
            });
        }
        Some(Cached::Revoked) => p.revoked = true,
        None => {}
    }
    show(app, view);
}

fn save(app: &tauri::AppHandle, p: &mut Progress, value: Option<Cached>) -> bool {
    let state = app.state::<AppState>();
    match cache::store(&state.sync_config.state_path, &p.binding, value.clone()) {
        Ok(true) => {
            apply_cache(app, p, value);
            true
        }
        Ok(false) => false,
        Err(error) => {
            tracing::warn!(%error, "could not update the known sources cache");
            if !matches!(value, Some(Cached::Known(_))) {
                apply_cache(app, p, value);
            }
            false
        }
    }
}

async fn current(state: &AppState, binding: &str) -> Option<Arc<ObserverClient>> {
    let c = pl_transport_win::access::load_current_client(&state.credential_access).await?;
    let sync = state.sync.lock().ok()?;
    (c.gate_open()
        && c.journal_identity().client_cert_sha256 == binding
        && sync.pairing.phase == PairingPhase::Paired
        && sync.pairing.binding == binding)
        .then_some(c)
}

fn failure(app: &tauri::AppHandle, p: &mut Progress, status: Option<u16>) -> bool {
    match status {
        Some(400) => {
            tracing::warn!("journal rejected the source registry request");
            false
        }
        Some(403) => {
            save(app, p, Some(Cached::Revoked));
            false
        }
        Some(404) => {
            p.unsupported = true;
            p.sent = None;
            save(app, p, None);
            false
        }
        _ => true,
    }
}

/// True requests a one-shot retry of failed work. Successful and unsupported
/// journals have no timer.
async fn sync(app: &tauri::AppHandle, p: &mut Progress, input: &Input, refresh: Refresh) -> bool {
    let state = app.state::<AppState>();
    if p.binding != input.binding {
        *p = Progress {
            binding: input.binding.clone(),
            ..Default::default()
        };
        let stored = (!input.binding.is_empty())
            .then(|| cache::read(&state.sync_config.state_path, &input.binding))
            .flatten();
        apply_cache(app, p, stored);
    }
    if input.binding.is_empty() || p.revoked {
        return false;
    }
    if input.advert == Some(SourceAdvert::Revoked) {
        return failure(app, p, Some(403));
    }
    if input.advert == Some(SourceAdvert::Unsupported) && !input.online && !p.unsupported {
        failure(app, p, Some(404));
    }
    if !input.online {
        p.connection = None;
        return false;
    }
    let Some(client) = current(&state, &p.binding).await else {
        return false;
    };
    let reconnect = p.connection != Some(input.connection);
    if reconnect {
        p.sent = None;
    }
    p.connection = Some(input.connection);
    let stale = now().saturating_sub(p.fetched_at) >= 300;
    let discover = reconnect || refresh == Refresh::Detail || (refresh == Refresh::Focus && stale);
    if discover || client.source_advert().is_none() {
        if let Err(error) = client.status_document().await {
            return match error {
                TransportError::Rejected { status, .. } => failure(app, p, Some(status)),
                _ => true,
            };
        }
    }
    let advert = client.source_advert();
    let changed = p.last_advert != advert;
    p.last_advert = advert.clone();
    let rev = match advert {
        Some(SourceAdvert::Available(rev)) => rev,
        Some(SourceAdvert::Unsupported) => {
            if !p.unsupported || p.rev.is_some() {
                failure(app, p, Some(404));
            }
            return false;
        }
        Some(SourceAdvert::Revoked) => return failure(app, p, Some(403)),
        None => return true,
    };
    if p.unsupported && !discover && !changed {
        return false;
    }
    p.unsupported = false;
    let mut retry = false;
    if let Some(local) = &input.local {
        if p.begin_declaration(local) {
            let Some(c) = current(&state, &p.binding).await else {
                return false;
            };
            let body = serde_json::to_vec(local).expect("source entries serialize");
            match c.shared_sources(Some(&body)).await.map(|r| r.status) {
                Ok(204) => p.sent = Some(local.clone()),
                Ok(400) => {
                    tracing::warn!(
                        "journal rejected the source declaration; waiting for a local change"
                    );
                    p.rejected = Some(local.clone());
                }
                Ok(status @ (403 | 404)) => return failure(app, p, Some(status)),
                _ => retry = true,
            }
        }
    }
    if p.rev.is_some()
        && (!changed && !reconnect || rev == p.rev)
        && refresh != Refresh::Detail
        && !(refresh == Refresh::Focus && stale)
    {
        return retry;
    }
    let Some(c) = current(&state, &p.binding).await else {
        return false;
    };
    match c.shared_sources(None).await {
        Ok(response) if response.status == 200 => {
            let Some(snapshot) = Snapshot::from_reply(&response.body, now()) else {
                return true;
            };
            if current(&state, &p.binding).await.is_none() {
                return false;
            }
            !save(app, p, Some(Cached::Known(snapshot))) || retry
        }
        result => failure(app, p, result.ok().map(|r| r.status)),
    }
}

async fn run(app: tauri::AppHandle, mut input: watch::Receiver<Input>) {
    let shared = app.state::<Shared>();
    let mut p = Progress::default();
    let mut delay = 0;
    let mut pending = Refresh::Cached;
    loop {
        tokio::select! {
            changed = input.changed() => if changed.is_err() {break},
            () = shared.wake.notified() => {},
            () = async { if delay == 0 {std::future::pending::<()>().await} else {
                tokio::time::sleep(Duration::from_secs(delay)).await;
            }} => {},
        }
        let next = input.borrow_and_update().clone();
        pending = pending.max(std::mem::replace(
            &mut *shared.refresh.lock().unwrap(),
            Refresh::Cached,
        ));
        if sync(&app, &mut p, &next, pending).await {
            delay = (delay * 2).clamp(15, 300);
            // Repeat discovery and the read if a transient error interrupted either.
            pending = Refresh::Detail;
        } else {
            delay = 0;
            pending = Refresh::Cached;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn declaration_projects_only_confirmed_sources() {
        use obsidian_source::State;
        let mut dump: HealthDump = serde_json::from_value(serde_json::json!({
            "app_state": "observing", "engine_ready": true, "version": "1", "sources": []
        }))
        .unwrap();
        dump.setup.screen = false;
        dump.setup.mic = false;
        dump.setup.system_audio = false;
        for state in [
            State::Off,
            State::Checking,
            State::Confirm,
            State::WaitingForJournalUpdate,
        ] {
            assert!(
                configured_sources(&dump, state, None, Some(ScreenshotsState::Choosing), None)
                    .is_empty()
            );
        }
        dump.setup.screen = true;
        dump.setup.system_audio = true;
        for kind in [SourceKind::Screen, SourceKind::SystemAudio] {
            dump.sources.push(observer_model::SourceReport {
                kind,
                state: SourceState::Active,
                device: None,
            });
        }
        let entries = configured_sources(
            &dump,
            State::Paused,
            Some("Notes".into()),
            Some(ScreenshotsState::FolderMissing),
            Some(("permitted", "ready")),
        );
        assert_eq!(
            serde_json::to_value(entries).unwrap(),
            serde_json::json!([
                {"source":"obsidian","state":"paused","detail":"Notes"},
                {"source":"screenshots","state":"needs_attention","detail":null},
                {"source":"screen","state":"on","detail":null},
                {"source":"audio","state":"on","detail":null},
                {"source":"browsers","state":"on","detail":null}
            ])
        );
    }

    #[test]
    fn an_ambiguous_replacement_resends_the_latest_selection() {
        let on = vec![Entry {
            source: "obsidian",
            state: "on",
            detail: Some("Notes".into()),
        }];
        let mut p = Progress {
            sent: Some(on.clone()),
            ..Default::default()
        };
        assert!(!p.begin_declaration(&on));
        assert!(p.begin_declaration(&[]));
        // The journal removed the vault but its response never arrived.
        assert!(p.begin_declaration(&on));
        p.sent = Some(on.clone());
        assert!(!p.begin_declaration(&on));
        p.rejected = Some(vec![]);
        assert!(!p.begin_declaration(&[]));
        assert!(!p.begin_declaration(&on));
        assert!(p.begin_declaration(&[]));
    }
}
