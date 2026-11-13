// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! The screenshots source, wired into the app: the paired upload path as the
//! source's journal seam, the computer's pause, and the commands the Settings
//! window drives it with.
//!
//! Everything scanned, held or scheduled lives in `screenshots-source`. What
//! is here carries no journal identity into anything it holds.

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use observer_model::{PauseReason, ScreenshotsState};
use pl_transport_win::source_upload::{upload_screenshot, ScreenshotFile, ScreenshotReply};
use pl_transport_win::{ObserverClient, TransportError};
use screenshots_source::{
    BoxFuture, Capability, Delivery, Journal, Pause, ScreenshotsDocument, ScreenshotsHandle, Span,
    Upload, CAPABILITY,
};
use tauri::Manager;

static HANDLE: OnceLock<ScreenshotsHandle> = OnceLock::new();

/// The folder dialog's title.
// PLACEHOLDER (copy gap): the locked copy names no title for the folder dialog.
// Proposed: D8's own second sentence, without its period. Gate before release.
const CHOOSER_TITLE: &str = "choose the folder your screenshots save to";

type LoadClient = Arc<
    dyn Fn() -> Pin<Box<dyn Future<Output = Option<Arc<ObserverClient>>> + Send>> + Send + Sync,
>;
type Heard = Arc<dyn Fn() + Send + Sync>;

/// The journal, over the app's paired upload path.
pub(crate) struct ClientJournal {
    load_client: LoadClient,
    heard: Heard,
    asked: Mutex<Option<Arc<ObserverClient>>>,
}

impl ClientJournal {
    /// `heard` runs whenever the journal answers, so a PC sending only
    /// screenshots still shows when it last reached the journal.
    pub(crate) fn new(load_client: LoadClient, heard: Heard) -> Self {
        Self {
            load_client,
            heard,
            asked: Mutex::new(None),
        }
    }
}

impl Journal for ClientJournal {
    fn capability(&self) -> BoxFuture<'_, Capability> {
        Box::pin(async move {
            let Some(client) = (self.load_client)().await else {
                *self.asked.lock().unwrap_or_else(|p| p.into_inner()) = None;
                return Capability::Unknown;
            };
            *self.asked.lock().unwrap_or_else(|p| p.into_inner()) = Some(Arc::clone(&client));
            let answer = client.system_capabilities().await;
            if matches!(answer, Ok(_) | Err(TransportError::Rejected { .. })) {
                (self.heard)();
            }
            match answer {
                Ok(listed) if listed.iter().any(|name| name == CAPABILITY) => Capability::Supported,
                Ok(_) => Capability::NotListed,
                Err(_) => Capability::Unknown,
            }
        })
    }

    fn deliver<'a>(&'a self, upload: &'a Upload) -> BoxFuture<'a, Delivery> {
        Box::pin(async move {
            let Some(client) = (self.load_client)().await else {
                return Delivery::NoAnswer;
            };
            let reply = upload_screenshot(
                &client,
                ScreenshotFile {
                    day: &upload.day,
                    segment: &upload.segment,
                    filename: &upload.filename,
                    content_type: upload.content_type,
                    captured_at: &upload.captured_at,
                    taken_from: upload.taken_from.as_str(),
                    bytes: upload.bytes.clone(),
                },
            )
            .await;
            delivery_for(reply, &self.heard)
        })
    }

    fn pairing_unchanged(&self) -> BoxFuture<'_, bool> {
        Box::pin(async move {
            let asked = self.asked.lock().unwrap_or_else(|p| p.into_inner()).clone();
            match (asked, (self.load_client)().await) {
                (Some(asked), Some(current)) => Arc::ptr_eq(&asked, &current),
                (None, None) => true,
                _ => false,
            }
        })
    }
}

/// How the app takes what the upload path got back. An answer is a delivery
/// the journal reported, so it moves when this PC last reached it.
fn delivery_for(reply: ScreenshotReply, heard: &Heard) -> Delivery {
    match reply {
        ScreenshotReply::Answered {
            http_status,
            status,
            reason_code,
        } => {
            heard();
            Delivery::Answered {
                http_status,
                status,
                reason_code,
            }
        }
        ScreenshotReply::Held | ScreenshotReply::Unreachable => Delivery::NoAnswer,
    }
}

fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

/// Open the source and start its work, off the setup path.
pub fn start(app: tauri::AppHandle, counters: Option<source_counters::SharedCounters>) {
    let client_app = app.clone();
    let load_client: LoadClient = Arc::new(move || {
        let app = client_app.clone();
        Box::pin(async move {
            let state = app.state::<crate::app::AppState>();
            pl_transport_win::access::load_current_client(&state.credential_access).await
        })
    });
    let heard_app = app.clone();
    let heard: Heard = Arc::new(move || {
        let state = heard_app.state::<crate::app::AppState>();
        if let Ok(mut sync) = state.sync.lock() {
            sync.upload.record_answer(now_millis());
        };
    });
    // The computer's pause, from the menu. A locked screen or a sleeping PC
    // takes no screenshots, so only the owner's pause opens a window.
    let paused_app = app.clone();
    let paused = Arc::new(move || {
        let state = paused_app.state::<crate::app::AppState>();
        let pause = state.health.lock().ok().and_then(|h| h.pause)?;
        (pause.reason == PauseReason::Operator).then(|| Pause {
            until_secs: pause
                .seconds_remaining
                .and_then(|secs| i64::try_from(secs).ok())
                .map(|secs| now_secs().saturating_add(secs)),
        })
    });
    let journal = ClientJournal::new(load_client, heard);
    let root = platform_win::local_data_root().join("screenshots");
    let platform = Arc::new(platform_win::screenshots::WindowsScreenshots);
    let (handle, work) = screenshots_source::start(root, platform, journal, paused, counters);
    let mut state = handle.state_changes();
    let _ = HANDLE.set(handle);
    tauri::async_runtime::spawn(work);
    // The state word goes into the health dump, for `--dump-state` and `/healthz`.
    tauri::async_runtime::spawn(async move {
        loop {
            let word = *state.borrow_and_update();
            let health = app.state::<crate::app::AppState>().health.clone();
            if let Ok(mut dump) = health.lock() {
                dump.screenshots_state = word;
            }
            if state.changed().await.is_err() {
                break;
            }
        }
    });
}

/// The owner confirmed a newly paired journal: first setup starts again.
pub fn restart_setup() {
    if let Some(handle) = handle() {
        handle.restart_setup();
    }
}

/// The source's state word, and when it changes.
pub fn state_changes() -> Option<tokio::sync::watch::Receiver<Option<ScreenshotsState>>> {
    handle().map(ScreenshotsHandle::state_changes)
}

fn handle() -> Option<&'static ScreenshotsHandle> {
    HANDLE.get()
}

fn to_value(document: Option<ScreenshotsDocument>) -> Option<serde_json::Value> {
    document.and_then(|document| serde_json::to_value(document).ok())
}

pub fn status() -> Option<serde_json::Value> {
    to_value(handle().and_then(ScreenshotsHandle::document))
}

/// The folder is gone: the one tray row names it.
pub fn needs_attention() -> bool {
    handle()
        .and_then(ScreenshotsHandle::status)
        .is_some_and(|status| status.state == ScreenshotsState::FolderMissing)
}

pub async fn turn_on() -> Option<serde_json::Value> {
    to_value(handle()?.turn_on().await)
}

pub async fn turn_off() -> Option<serde_json::Value> {
    to_value(handle()?.turn_off().await)
}

pub async fn confirm(span: Span) -> Option<serde_json::Value> {
    to_value(handle()?.confirm(span).await)
}

pub async fn try_again() -> Option<serde_json::Value> {
    to_value(handle()?.try_again().await)
}

/// Ask the owner for a folder with the native dialog and read that one.
/// `Ok(None)` when the owner cancels the dialog.
pub async fn choose_folder(
    window: tauri::WebviewWindow,
) -> Result<Option<serde_json::Value>, String> {
    crate::logs::require_settings(&window)?;
    let owner = window
        .hwnd()
        .map_err(|_| "screenshots_window_unavailable")?
        .0 as isize;
    let chosen = tauri::async_runtime::spawn_blocking(move || {
        platform_win::folder_picker::choose_folder(owner, CHOOSER_TITLE)
    })
    .await
    .map_err(|_| "screenshots_dialog_failed")?
    .map_err(|_| "screenshots_dialog_failed")?;
    let Some(path) = chosen else {
        return Ok(None);
    };
    let Some(path) = path.to_str().map(str::to_owned) else {
        return Err("screenshots_unusable_path".into());
    };
    let Some(handle) = handle() else {
        return Ok(None);
    };
    Ok(to_value(handle.choose_folder(path).await))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn an_answer_is_a_delivery_reported_and_no_answer_is_not() {
        let count = Arc::new(AtomicUsize::new(0));
        let seen = Arc::clone(&count);
        let heard: Heard = Arc::new(move || {
            seen.fetch_add(1, Ordering::SeqCst);
        });
        let answered = delivery_for(
            ScreenshotReply::Answered {
                http_status: 200,
                status: Some("ok".into()),
                reason_code: None,
            },
            &heard,
        );
        assert!(matches!(
            answered,
            Delivery::Answered {
                http_status: 200,
                ..
            }
        ));
        assert_eq!(count.load(Ordering::SeqCst), 1);
        assert_eq!(
            delivery_for(ScreenshotReply::Held, &heard),
            Delivery::NoAnswer
        );
        assert_eq!(
            delivery_for(ScreenshotReply::Unreachable, &heard),
            Delivery::NoAnswer
        );
        assert_eq!(count.load(Ordering::SeqCst), 1);
    }
}
