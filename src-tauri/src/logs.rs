// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Settings owns one immutable document from read through Save As.

use std::io::Write;

// The rotation family is 5 x 5 MiB. Reserve room for the document notes while
// retaining the full family within one bounded IPC response. Windows rate
// measurements and the owner-use margin are recorded with the change.
const MAX_DOCUMENT_BYTES: usize = 25 * 1024 * 1024;
const HEADER_RESERVE: usize = 4096;
const MAX_ENTRIES: usize = 2_000_000;

fn load_document() -> Result<(String, &'static str, usize), String> {
    use observer_log::format_rfc3339_utc as timestamp;
    use observer_log::preview::{self, DirectorySource, MemberStatus, PreviewLimits};

    let source = DirectorySource::new(platform_win::logs_dir(), observer_log::persistent_writer());
    let snapshot = preview::read(
        &source,
        &PreviewLimits {
            anchor: std::time::SystemTime::now(),
            hours: 24,
            max_entries: MAX_ENTRIES,
            max_bytes: MAX_DOCUMENT_BYTES - HEADER_RESERVE,
        },
    )
    .map_err(|_| "logs_read_failed")?;
    let meta = snapshot.meta();
    let unreadable = meta
        .members
        .iter()
        .any(|m| m.status == MemberStatus::Unreadable);
    let partial = meta
        .members
        .iter()
        .any(|m| m.status == MemberStatus::Partial);
    let all_missing = meta
        .members
        .iter()
        .all(|m| m.status == MemberStatus::Missing);
    let missing_gap = meta
        .members
        .iter()
        .rposition(|m| m.status != MemberStatus::Missing)
        .is_some_and(|last| {
            meta.members[..=last]
                .iter()
                .any(|m| m.status == MemberStatus::Missing)
        });
    let has_gaps = unreadable
        || partial
        || missing_gap
        || meta.legacy > 0
        || meta.malformed > 0
        || !meta.consistent;
    let state = if meta.count == 0 {
        if unreadable {
            "unreadable"
        } else if all_missing {
            "missing"
        } else {
            "empty"
        }
    } else if has_gaps || meta.truncated {
        "partial"
    } else {
        "ready"
    };
    let yes_no = |value| if value { "yes" } else { "no" };
    let first = meta.first.map(timestamp).unwrap_or_else(|| "none".into());
    let last = meta.last.map(timestamp).unwrap_or_else(|| "none".into());
    let start_reached = meta.history_reaches_start && !has_gaps && !meta.truncated;
    let mut document = format!(
        "solstone recent logs\napp version: {}\nplatform: windows\nrequested history: {} hours before {}\nmaximum entries: {}\nmaximum document bytes: {}\nmaximum bytes read per file: {}\nrotation capacity: {} files of {} bytes\nentries shown: {}\nfirst entry: {}\nlast entry: {}\nrequested start reached without gaps: {}\ntruncated: {}\nconsistent file snapshot: {}\nolder-format lines skipped: {}\nmalformed lines skipped: {}\n",
        env!("CARGO_PKG_VERSION"), meta.hours, timestamp(meta.anchor),
        meta.max_entries, MAX_DOCUMENT_BYTES, observer_log::MAX_BYTES * 2,
        observer_log::MAX_FILES, observer_log::MAX_BYTES, meta.count, first, last,
        yes_no(start_reached), yes_no(meta.truncated || partial), yes_no(meta.consistent),
        meta.legacy, meta.malformed,
    );
    for member in &meta.members {
        let name = if member.index == 0 {
            "solstone.log".into()
        } else {
            format!("solstone.{}.log", member.index)
        };
        let status = match member.status {
            MemberStatus::Missing => "missing",
            MemberStatus::Empty => "empty",
            MemberStatus::Unreadable => "unreadable",
            MemberStatus::Partial => "only the end was read",
            MemberStatus::Read => "read",
        };
        document.push_str(&format!("{name}: {status}\n"));
    }
    document.push_str("older files are replaced as logs grow, so less than the requested history may be available.\n\n");
    if document.len() > HEADER_RESERVE {
        return Err("logs_document_failed".into());
    }
    document.push_str(std::str::from_utf8(snapshot.bytes()).map_err(|_| "logs_document_failed")?);
    Ok((document, state, meta.count))
}

#[derive(Clone, serde::Serialize)]
pub struct LogPreview {
    id: u64,
    document: String,
    state: &'static str,
    entries: usize,
}

#[derive(Default)]
struct PreviewStore {
    sequence: u64,
    preview: Option<LogPreview>,
}

#[derive(Default)]
pub struct LogExports(tokio::sync::Mutex<PreviewStore>);

fn require_settings(window: &tauri::WebviewWindow) -> Result<(), String> {
    if window.label() == "settings" {
        Ok(())
    } else {
        Err("settings_required".into())
    }
}

#[tauri::command]
pub async fn read_recent_logs(
    window: tauri::WebviewWindow,
    exports: tauri::State<'_, LogExports>,
) -> Result<LogPreview, String> {
    require_settings(&window)?;
    let mut store = exports.inner().0.try_lock().map_err(|_| "logs_busy")?;
    store.preview = None;
    let (document, state, entries) = tauri::async_runtime::spawn_blocking(load_document)
        .await
        .map_err(|_| "logs_read_failed")??;
    store.sequence += 1;
    let preview = LogPreview {
        id: store.sequence,
        document,
        state,
        entries,
    };
    store.preview = Some(preview.clone());
    Ok(preview)
}

#[tauri::command]
pub async fn save_recent_logs(
    window: tauri::WebviewWindow,
    exports: tauri::State<'_, LogExports>,
    preview_id: u64,
) -> Result<bool, String> {
    require_settings(&window)?;
    let store = exports.inner().0.try_lock().map_err(|_| "logs_busy")?;
    let preview = store
        .preview
        .as_ref()
        .filter(|p| p.id == preview_id && p.entries > 0)
        .ok_or("logs_preview_expired")?;
    let owner = window.hwnd().map_err(|_| "logs_window_unavailable")?.0 as isize;
    let document = preview.document.clone();
    // Keep the store locked through the dialog and write: a concurrent read
    // cannot replace the reviewed snapshot while Save As is open.
    let result = tauri::async_runtime::spawn_blocking(move || {
        let Some(destination) =
            platform_win::log_save::choose_destination(owner).map_err(|_| "logs_dialog_failed")?
        else {
            return Ok(false);
        };
        let mut file = std::fs::File::create(destination).map_err(|_| "logs_write_failed")?;
        file.write_all(document.as_bytes())
            .map_err(|_| "logs_write_failed")?;
        file.flush().map_err(|_| "logs_write_failed")?;
        Ok(true)
    })
    .await
    .map_err(|_| "logs_save_failed")?;
    drop(store);
    result.map_err(str::to_owned)
}
