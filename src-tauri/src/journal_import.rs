// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Import handoff for the journal window: the journal's import page asks the app
//! to pick files, and a file dropped on the window goes to the page as a token.
//! Picked and dropped files go into the app's own import queue, the one Settings
//! uses, and upload from there whether or not the window stays open.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use journal_window::auth_host::{self, AuthBroker, AuthMessage};
use journal_window::import_host::{
    self, Accepted, Declined, ImportContract, Outcome, Pick, Request,
};
use journal_window::JournalOrigin;
use observer_model::i18n::message;
use pl_transport_win::import_client::Handoff;
use serde_json::Value;
use tauri::{Manager, WebviewWindow};

/// One journal window's side of the channel.
pub struct Host {
    pub contract: ImportContract,
    auth: Option<Arc<AuthBroker>>,
    nonce: String,
    busy: AtomicBool,
    /// The journal's pages carry this contract, so a drop can go to them. A
    /// window shows one journal, so this holds for the window's life.
    ready: AtomicBool,
    dropped: Mutex<Option<(String, Vec<PathBuf>)>>,
}

impl Host {
    /// The host and the script that adapts the page to it, or `None` when the
    /// bundled contract cannot be read; then the page uploads on its own.
    pub fn new(app: &tauri::AppHandle, origin: &JournalOrigin) -> Option<(Arc<Self>, String)> {
        let contract = match import_host::import_contract(import_host::bundled_import_contract()) {
            Ok(contract) => contract,
            Err(error) => {
                tracing::warn!(target: "window", label = "journal", token = error.token(), "journal import contract");
                return None;
            }
        };
        let auth = match auth_host::auth_contract(auth_host::bundled_auth_contract()) {
            Ok(auth_contract) if auth_contract.capability == contract.capability => {
                let open_app = app.clone();
                Some(Arc::new(AuthBroker::new(
                    auth_contract,
                    move |url| {
                        use tauri_plugin_opener::OpenerExt;
                        open_app
                            .opener()
                            .open_url(url, None::<&str>)
                            .map_err(|_| ())
                    },
                    || pl_transport_win::import_store::new_id().ok(),
                )))
            }
            Ok(_) => {
                tracing::warn!(target: "window", label = "journal", "journal auth contract capability mismatch");
                None
            }
            Err(error) => {
                tracing::warn!(target: "window", label = "journal", token = error.token(), "journal auth contract");
                None
            }
        };
        let nonce = pl_transport_win::import_store::new_id().ok()?;
        let script = import_host::page_script_with_auth(
            import_host::bundled_import_contract(),
            auth.as_ref().map(|_| auth_host::bundled_auth_contract()),
            origin,
            &nonce,
        );
        Some((
            Arc::new(Self {
                contract,
                auth,
                nonce,
                busy: AtomicBool::new(false),
                ready: AtomicBool::new(false),
                dropped: Mutex::new(None),
            }),
            script,
        ))
    }

    /// Cancels any active OAuth loopback session and settles any pending
    /// `auth.authorize` request as `cancelled`.
    pub fn tear_down_auth(&self) {
        if let Some(broker) = &self.auth {
            broker.tear_down();
        }
    }
}

/// A request from the page, by the channel's id. Called on the window's UI
/// thread; the work runs on the async runtime and the answer goes back to that
/// same request.
pub fn receive(app: &tauri::AppHandle, label: &str, host: &Arc<Host>, id: u64, header: String) {
    let Some(window) = app.get_webview_window(label) else {
        return;
    };
    let Some(request) = import_host::parse_with_auth(
        &host.contract,
        host.auth.as_ref().map(|broker| broker.contract()),
        &header,
        &host.nonce,
    ) else {
        answer_request(&window, id, None);
        return;
    };
    if let Request::Auth(auth_message) = request {
        let Some(broker) = host.auth.clone() else {
            let msg_id = match &auth_message {
                AuthMessage::Prepare { id, .. }
                | AuthMessage::Authorize { id, .. }
                | AuthMessage::Cancel { id, .. } => id.as_str(),
            };
            let reply = import_host::unsupported_reply(&host.contract, msg_id);
            answer_request(&window, id, Some(reply));
            return;
        };
        tauri::async_runtime::spawn_blocking(move || {
            let reply = broker.handle(auth_message);
            answer_request(&window, id, Some(reply));
        });
        return;
    }
    // Requests arrive here in the order the page sent them, so a pick claims the
    // busy flag here rather than in its task: a second pick sent while the
    // picker is open is always the one told busy.
    if let Request::Pick(pick) = &request {
        if host.busy.swap(true, Ordering::AcqRel) {
            let reply =
                import_host::pick_reply(&host.contract, &pick.id, Outcome::Busy, &[], &[], None);
            answer_request(&window, id, Some(reply));
            return;
        }
    }
    let app = app.clone();
    let host = host.clone();
    tauri::async_runtime::spawn(async move {
        let reply = answer(&app, &window, &host, request).await;
        answer_request(&window, id, Some(reply));
    });
}

async fn answer(
    app: &tauri::AppHandle,
    window: &WebviewWindow,
    host: &Host,
    request: Request,
) -> Value {
    let c = &host.contract;
    match request {
        Request::Ready => {
            host.ready.store(true, Ordering::Release);
            serde_json::json!({})
        }
        Request::Status(Some(item)) => crate::import::handoff_status(app, &item),
        Request::Status(None) => serde_json::json!({"state": "gone"}),
        Request::Unsupported(id) => import_host::unsupported_reply(c, &id),
        Request::Refused(id) => import_host::pick_reply(c, &id, Outcome::Refused, &[], &[], None),
        Request::Auth(auth_message) => {
            let msg_id = match &auth_message {
                AuthMessage::Prepare { id, .. }
                | AuthMessage::Authorize { id, .. }
                | AuthMessage::Cancel { id, .. } => id.as_str(),
            };
            import_host::unsupported_reply(c, msg_id)
        }
        Request::Pick(pick) => {
            // `receive` claimed the busy flag for this request.
            let reply = take_files(app, window, host, pick).await;
            host.busy.store(false, Ordering::Release);
            reply
        }
    }
}

async fn take_files(
    app: &tauri::AppHandle,
    window: &WebviewWindow,
    host: &Host,
    pick: Pick,
) -> Value {
    let c = &host.contract;
    let refused = |reason: Option<String>| {
        import_host::pick_reply(c, &pick.id, Outcome::Refused, &[], &[], reason.as_deref())
    };
    let paths = if let Some(token) = &pick.drop {
        let mut dropped = host.dropped.lock().unwrap_or_else(|p| p.into_inner());
        match dropped.take() {
            Some((held, paths)) if &held == token => paths,
            other => {
                *dropped = other;
                return refused(None);
            }
        }
    } else {
        let Ok(owner) = window.hwnd().map(|hwnd| hwnd.0 as isize) else {
            return refused(None);
        };
        let filter = import_host::picker_filter(&pick.accept)
            .map(|(name, pattern)| platform_win::file_picker::Filter { name, pattern });
        let multiple = pick.multiple;
        let chosen = tauri::async_runtime::spawn_blocking(move || {
            platform_win::file_picker::choose_files(owner, filter.as_ref(), multiple)
        })
        .await;
        match chosen {
            Ok(Ok(Some(paths))) if !paths.is_empty() => paths,
            Ok(Ok(_)) => {
                return import_host::pick_reply(c, &pick.id, Outcome::Cancelled, &[], &[], None)
            }
            _ => return refused(None),
        }
    };
    let handoff = Handoff {
        area: pick.area,
        context: pick.context,
    };
    let Some((accepted, declined)) = crate::import::add_handoff(app, paths, handoff).await else {
        return refused(Some(message(
            "windows.ui.couldntreadimportsonthisdevicenothinghasbeenremo",
            &[],
        )));
    };
    let accepted: Vec<_> = accepted
        .into_iter()
        .map(|(item, name)| Accepted { item, name })
        .collect();
    let declined: Vec<_> = declined
        .into_iter()
        .map(|d| Declined {
            name: d.name,
            reason: d.reason,
        })
        .collect();
    let outcome = if accepted.is_empty() {
        Outcome::Refused
    } else {
        Outcome::Queued
    };
    import_host::pick_reply(c, &pick.id, outcome, &accepted, &declined, None)
}

/// Files dropped on the journal window. A page that carries the contract gets
/// a token for them, which the card under the drop, or a quick import, spends.
pub fn dropped(window: &WebviewWindow, host: &Host, paths: Vec<PathBuf>) {
    if paths.is_empty() || !host.ready.load(Ordering::Acquire) {
        return;
    }
    let Ok(token) = pl_transport_win::import_store::new_id() else {
        return;
    };
    *host.dropped.lock().unwrap_or_else(|p| p.into_inner()) = Some((token.clone(), paths));
    if let Err(error) = window.eval(import_host::drop_script(&host.contract, &token)) {
        tracing::warn!(target: "window", label = "journal", %error, "journal drop could not reach the page");
    }
}

fn answer_request(window: &WebviewWindow, id: u64, reply: Option<Value>) {
    let header = reply.as_ref().map(import_host::reply_header);
    #[cfg(windows)]
    {
        let result = window.with_webview(move |webview| {
            if !platform_win::journal_import_channel::respond(
                &webview.environment(),
                id,
                header.as_deref(),
            ) {
                tracing::warn!(target: "window", label = "journal", "journal import answer was not delivered");
            }
        });
        if result.is_err() {
            tracing::warn!(target: "window", label = "journal", "journal import answer had no window");
        }
    }
    #[cfg(not(windows))]
    let _ = (window, id, header);
}
