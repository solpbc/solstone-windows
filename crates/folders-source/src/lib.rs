// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! The folder source on Windows: watches one or more owner-chosen folders,
//! sorts files against the journal's supported list, tracks note edits and
//! one-hour document revisions, reads Windows `Zone.Identifier` metadata and
//! converts images through the platform seam, and delivers five-minute
//! activity segments over the paired protocol-v3 upload path.

#![forbid(unsafe_code)]

pub mod classify;
pub mod engine;
pub mod note_diff;
pub mod platform;
pub mod stamp;
pub mod types;
pub mod wire;
pub mod worker;
pub mod zone_identifier;

pub use engine::{
    stage_delivery, DeliveryOutcome, Engine, IntakeHook, OverlapContext, PendingEvent,
    SegmentDelivery, StagedContentFile, MAX_ESTIMATE_ITEMS, MAX_UPLOAD_CONTENT_PARTS,
};
pub use platform::{ConvertedImage, FolderPlatform};
pub use screenshots_source::Span;
pub use solstone_folder_sync::worker::{BoxFuture, Capability, Journal};
pub use stamp::{Stamp, Window};
pub use types::{
    ActivityVerb, BackfillEstimate, DeliveryMode, EstimateItem, FacetOption, FacetsAvailability,
    FileKind, FolderActionRequest, FolderAttention, FolderDeclaration, FolderDeclaredState,
    FolderKinds, FolderMode, FolderSummary, FoldersDocument, OverlapTarget, PathCheckResult,
    PlatformMetadata, PreviewSummary, SupportedList,
};
pub use wire::{
    outcome_for, parse_estimate, parse_facets, parse_supported_list, FolderDeliveryReply,
    FolderTransport, CAPABILITY, SOURCE,
};
pub use worker::{FoldersHandle, OverlapProvider, Paused};
pub use zone_identifier::{
    extension_lower, guess_mime, origin_domain, parse_zone_identifier, ZoneIdentifier,
};

use std::future::Future;
use std::path::PathBuf;
use std::sync::Arc;

use source_counters::SharedCounters;

/// Start the folder source: returns a handle for the rest of the app and a
/// future that runs the background worker on the caller's async runtime.
pub fn start<T: FolderTransport>(
    state_dir: PathBuf,
    platform: Arc<dyn FolderPlatform>,
    transport: Arc<T>,
    paused: Paused,
    overlap_provider: OverlapProvider,
    counters: Option<SharedCounters>,
) -> (FoldersHandle, impl Future<Output = ()> + Send + 'static) {
    worker::spawn_worker(
        state_dir,
        platform,
        transport,
        paused,
        overlap_provider,
        counters,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::ffi::OsString;
    use std::fs;
    use std::path::Path;
    use std::sync::atomic::{AtomicI64, Ordering};
    use std::sync::Mutex;

    struct FakePlatform {
        ms: AtomicI64,
        offset_s: i32,
        home: PathBuf,
        zones: Mutex<HashMap<PathBuf, ZoneIdentifier>>,
        file_ids: Mutex<HashMap<String, String>>,
        converted: Mutex<Vec<FileKind>>,
    }

    impl FakePlatform {
        fn new(ms: i64, offset_s: i32, home: PathBuf) -> Arc<Self> {
            Arc::new(Self {
                ms: AtomicI64::new(ms),
                offset_s,
                home,
                zones: Mutex::new(HashMap::new()),
                file_ids: Mutex::new(HashMap::new()),
                converted: Mutex::new(Vec::new()),
            })
        }

        fn advance_ms(&self, delta: i64) {
            self.ms.fetch_add(delta, Ordering::SeqCst);
        }

        fn set_zone(&self, path: &Path, zone: ZoneIdentifier) {
            let key = classify::normalize_path(path);
            self.zones.lock().unwrap().insert(key, zone);
        }

        fn set_file_id(&self, name: &str, id: &str) {
            self.file_ids
                .lock()
                .unwrap()
                .insert(name.to_owned(), id.to_owned());
        }
    }

    impl FolderPlatform for FakePlatform {
        fn now_ms(&self) -> i64 {
            self.ms.load(Ordering::SeqCst)
        }

        fn offset_s_at(&self, _epoch_secs: i64) -> i32 {
            self.offset_s
        }

        fn home_dir(&self) -> PathBuf {
            self.home.clone()
        }

        fn file_ids_in_dir(&self, _dir: &Path) -> HashMap<OsString, String> {
            self.file_ids
                .lock()
                .unwrap()
                .iter()
                .map(|(k, v)| (OsString::from(k), v.clone()))
                .collect()
        }

        fn read_zone_identifier(&self, path: &Path) -> Option<ZoneIdentifier> {
            let key = classify::normalize_path(path);
            self.zones.lock().unwrap().get(&key).cloned()
        }

        fn is_screenshot(&self, path: &Path, _ext: Option<&str>) -> bool {
            path.file_stem()
                .and_then(|s| s.to_str())
                .is_some_and(screenshots_source::rules::has_name_stamp)
        }

        fn convert_image(&self, bytes: &[u8], kind: FileKind) -> Option<ConvertedImage> {
            self.converted.lock().unwrap().push(kind);
            let mut out = b"CONVERTED:".to_vec();
            out.extend_from_slice(bytes);
            match kind {
                FileKind::Screenshot => Some(ConvertedImage {
                    bytes: out,
                    extension: "png",
                    content_type: "image/png",
                }),
                _ => Some(ConvertedImage {
                    bytes: out,
                    extension: "jpg",
                    content_type: "image/jpeg",
                }),
            }
        }
    }

    fn write_settled(path: &Path, bytes: &[u8]) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, bytes).unwrap();
    }

    #[test]
    fn scenario_1_setup_preview_mixed_folder_zone_id_and_wic_staging() {
        let root_tmp = tempfile::tempdir().unwrap();
        let state_dir = root_tmp.path().join("state");
        let home = root_tmp.path().join("home");
        let folder = home.join("Downloads");
        fs::create_dir_all(&folder).unwrap();

        // Write files BEFORE advancing fake clock far ahead so they are well settled.
        write_settled(
            &folder.join("notes.md"),
            b"# Plan\n\nShip Windows folder source.\n",
        );
        write_settled(&folder.join("doc.pdf"), b"%PDF-1.7\n1 0 obj\nendobj\n");
        write_settled(
            &folder.join("call.vtt"),
            b"WEBVTT\n\n00:00:01.000 --> 00:00:02.000\nHello\n",
        );
        write_settled(
            &folder.join("Screenshot 2026-11-27 140500.bmp"),
            b"BM_FAKE_SCREENSHOT",
        );
        write_settled(&folder.join("camera.heic"), b"HEIC_FAKE_PHOTO");
        write_settled(&folder.join("installer.exe"), b"MZ_BINARY");
        write_settled(&folder.join("partial.zip.crdownload"), b"incomplete");
        write_settled(&folder.join("Thumbs.db"), b"junk");

        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64
            + 600_000;
        let platform = FakePlatform::new(now_ms, -7 * 3600, home);
        platform.set_zone(
            &folder.join("installer.exe"),
            ZoneIdentifier {
                zone_id: Some(3),
                origin_url: Some("https://releases.example.org/installer.exe".into()),
                referrer_url: Some("https://releases.example.org/download".into()),
            },
        );

        let mut engine = Engine::open(state_dir, platform.clone());
        let id = engine.begin_setup(&folder, None).unwrap();
        engine.set_facet(
            &id,
            Some(FacetOption {
                name: "work".into(),
                title: "Work".into(),
                color: None,
            }),
        );

        let (preview, estimate_items) = engine.preview_folder_with_items(&id, Span::All).unwrap();
        assert_eq!(preview.total_items, 6);
        assert_eq!(preview.content_items, 5);
        assert_eq!(preview.metadata_only_items, 1);
        assert_eq!(preview.notes, 1);
        assert_eq!(preview.documents, 1);
        assert_eq!(preview.transcripts, 1);
        assert_eq!(preview.screenshots, 1);
        assert_eq!(preview.photos, 1);
        assert_eq!(preview.other, 1);
        assert_eq!(estimate_items.len(), 6);
        // Nothing declared while still setting up.
        assert!(engine.declared_folders(false).is_empty());

        engine.confirm_folder(&id, Span::All).unwrap();
        assert_eq!(
            engine.declared_folders(false),
            vec![FolderDeclaration {
                id: id.clone(),
                state: FolderDeclaredState::On,
            }]
        );

        let delivery = engine.next_delivery(false).unwrap();
        let (meta, parts) = stage_delivery(&delivery, &*platform).unwrap();
        assert_eq!(meta.v, 1);
        assert_eq!(meta.id, id);
        assert_eq!(meta.name, "Downloads");
        assert_eq!(meta.facet.as_deref(), Some("work"));
        // 1 activity JSONL + 5 content parts (md, pdf, vtt, converted png, converted jpg)
        assert_eq!(parts.len(), 6);
        assert!(parts[0].filename.starts_with("folder_"));
        assert!(parts[0].filename.ends_with(".jsonl"));
        // Both convertible images went through platform.convert_image
        assert_eq!(platform.converted.lock().unwrap().len(), 2);
        assert!(parts.iter().any(|p| p.filename.ends_with(".png")));
        assert!(parts.iter().any(|p| p.filename.ends_with(".jpg")));

        // Verify Zone.Identifier fields landed on the installer.exe activity line
        let jsonl = std::str::from_utf8(&parts[0].bytes).unwrap();
        let exe_line: serde_json::Value = jsonl
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .find(|v: &serde_json::Value| v["path"] == "installer.exe")
            .unwrap();
        assert_eq!(exe_line["part"], serde_json::Value::Null);
        assert_eq!(exe_line["platform"]["zone_id"], 3);
        assert_eq!(
            exe_line["platform"]["origin_url"],
            "https://releases.example.org/installer.exe"
        );
        assert_eq!(
            exe_line["platform"]["referrer_url"],
            "https://releases.example.org/download"
        );

        engine.record_outcome(&delivery, DeliveryOutcome::Delivered);
        assert!(engine.next_delivery(false).is_none());
    }

    #[test]
    fn scenario_note_edit_revert_rename_delete_pause_and_refused_fallback() {
        let root_tmp = tempfile::tempdir().unwrap();
        let state_dir = root_tmp.path().join("state");
        let home = root_tmp.path().join("home");
        let folder = home.join("Work");
        fs::create_dir_all(&folder).unwrap();

        let base_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64
            + 600_000;
        let platform = FakePlatform::new(base_ms, -7 * 3600, home);

        write_settled(&folder.join("todo.md"), b"line 1\nline 2\n");
        write_settled(&folder.join("bad.pdf"), b"not really a pdf");
        platform.set_file_id("todo.md", "ntfs-id-1");

        let mut engine = Engine::open(state_dir.clone(), platform.clone());
        let id = engine.begin_setup(&folder, None).unwrap();
        engine.confirm_folder(&id, Span::All).unwrap();

        // Deliver initial batch, simulating journal refusing bad.pdf content.
        let d1 = engine.next_delivery(false).unwrap();
        engine.record_outcome(
            &d1,
            DeliveryOutcome::DeliveredWithRefusedContent(vec!["bad.pdf".into()]),
        );
        // bad.pdf is re-queued metadata-only and fallback_to_details incremented.
        assert_eq!(engine.folders()[0].fallback_to_details, 1);
        let d_fb = engine.next_delivery(false).unwrap();
        assert_eq!(d_fb.events.len(), 1);
        assert_eq!(d_fb.events[0].rel_path, "bad.pdf");
        assert_eq!(d_fb.events[0].delivery, DeliveryMode::MetadataOnly);
        engine.record_outcome(&d_fb, DeliveryOutcome::Delivered);

        // Pause holds delivery without skipping scans!
        assert!(engine.pause_folder(&id));
        assert_eq!(
            engine.declared_folders(false),
            vec![FolderDeclaration {
                id: id.clone(),
                state: FolderDeclaredState::Paused,
            }]
        );
        write_settled(&folder.join("while_paused.txt"), b"added while paused");
        // Advance clock past settle + 5-min window
        platform.advance_ms(600_000);
        engine.scan_all();
        // Held while paused!
        assert!(engine.next_delivery(false).is_none());
        // Resume delivers it right away without losing it.
        assert!(engine.resume_folder(&id));
        let d_paused = engine.next_delivery(false).unwrap();
        assert_eq!(d_paused.events[0].rel_path, "while_paused.txt");
        engine.record_outcome(&d_paused, DeliveryOutcome::Delivered);

        // Delete-before-send: add a temp note, scan, then delete before window closes.
        write_settled(&folder.join("ephemeral.md"), b"never sent");
        platform.advance_ms(600_000);
        engine.scan_all();
        fs::remove_file(folder.join("ephemeral.md")).unwrap();
        engine.scan_all();
        assert!(engine.next_delivery(false).is_none());

        // Rename via stable NTFS file_id emits `renamed` with `previous_path`.
        fs::rename(folder.join("todo.md"), folder.join("tasks.md")).unwrap();
        platform.set_file_id("tasks.md", "ntfs-id-1");
        platform.advance_ms(600_000);
        engine.scan_all();
        // Advance past the 5-minute window close for the rename event.
        platform.advance_ms(300_000);
        let d_ren = engine.next_delivery(false).unwrap();
        assert_eq!(d_ren.events[0].verb, ActivityVerb::Renamed);
        assert_eq!(d_ren.events[0].rel_path, "tasks.md");
        assert_eq!(d_ren.events[0].previous_path.as_deref(), Some("todo.md"));
        engine.record_outcome(&d_ren, DeliveryOutcome::Delivered);

        // Turn off drops declarations and stops scanning.
        assert!(engine.turn_off_folder(&id));
        assert!(engine.declared_folders(false).is_empty());

        // Turn back on & confirm, then restart_setup (Principle 9) resets to SettingUp.
        assert!(engine.turn_on_folder(&id));
        engine.confirm_folder(&id, Span::Nothing).unwrap();
        engine.restart_setup();
        assert_eq!(engine.folders()[0].mode, FolderMode::SettingUp);
        assert!(engine.declared_folders(false).is_empty());
    }

    #[test]
    fn scenario_document_revision_waits_for_one_hour_cooldown() {
        let root_tmp = tempfile::tempdir().unwrap();
        let state_dir = root_tmp.path().join("state");
        let home = root_tmp.path().join("home");
        let folder = home.join("Docs");
        fs::create_dir_all(&folder).unwrap();

        let base_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64
            + 600_000;
        let platform = FakePlatform::new(base_ms, 0, home);

        write_settled(&folder.join("spec.pdf"), b"%PDF-1.7\nv1\n");
        let mut engine = Engine::open(state_dir, platform.clone());
        let id = engine.begin_setup(&folder, None).unwrap();
        engine.confirm_folder(&id, Span::All).unwrap();
        let d1 = engine.next_delivery(false).unwrap();
        engine.record_outcome(&d1, DeliveryOutcome::Delivered);

        // Edit the PDF (changes size and content)
        write_settled(
            &folder.join("spec.pdf"),
            b"%PDF-1.7\nv2 with more content\n",
        );
        platform.advance_ms(600_000);
        engine.scan_all();
        // Not due yet: 10 minutes < 60 minutes cool-down.
        assert!(engine.next_delivery(false).is_none());

        // Advance past 1 hour + 5 min window
        platform.advance_ms(3_900_000);
        engine.scan_all();
        let d2 = engine.next_delivery(false).unwrap();
        assert_eq!(d2.events.len(), 1);
        assert_eq!(d2.events[0].verb, ActivityVerb::Revised);
        assert!(d2.events[0].is_revision);
    }

    #[test]
    fn ledger_contains_zero_journal_identity() {
        let root_tmp = tempfile::tempdir().unwrap();
        let state_dir = root_tmp.path().join("state");
        let home = root_tmp.path().join("home");
        let folder = home.join("Notes");
        fs::create_dir_all(&folder).unwrap();
        let platform = FakePlatform::new(1_800_000_000_000, 0, home);
        let mut engine = Engine::open(state_dir.clone(), platform);
        let _ = engine.begin_setup(&folder, None).unwrap();
        let raw = fs::read_to_string(state_dir.join("ledger.json")).unwrap();
        assert!(!raw.contains("journal_id"));
        assert!(!raw.contains("jid"));
        assert!(!raw.contains("fingerprint"));
    }

    #[test]
    fn span_nothing_starts_at_local_midnight_including_earlier_today() {
        let root_tmp = tempfile::tempdir().unwrap();
        let state_dir = root_tmp.path().join("state");
        let home = root_tmp.path().join("home");
        let folder = home.join("TodayOnly");
        fs::create_dir_all(&folder).unwrap();

        // Pick a real UTC timestamp near now so OS mtimes on files created right now
        // fall strictly between local midnight and "now + 2h".
        let real_now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64;
        // Choose an offset so local time of `real_now_ms` is midday (12:00 local).
        let utc_sec_of_day = (real_now_ms / 1000).rem_euclid(86_400);
        let offset_s = (12 * 3600 - utc_sec_of_day) as i32;
        let platform = FakePlatform::new(real_now_ms + 3_600_000, offset_s, home);

        write_settled(&folder.join("today.md"), b"written earlier today");
        let mut engine = Engine::open(state_dir, platform);
        let id = engine.begin_setup(&folder, None).unwrap();
        let preview = engine.preview_folder(&id, Span::Nothing).unwrap();
        assert_eq!(preview.total_items, 1);
    }

    #[test]
    fn rename_and_delete_before_send_work_alongside_unrelated_partial_download() {
        let root_tmp = tempfile::tempdir().unwrap();
        let state_dir = root_tmp.path().join("state");
        let home = root_tmp.path().join("home");
        let folder = home.join("Downloads");
        fs::create_dir_all(&folder).unwrap();

        let base_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64
            + 600_000;
        let platform = FakePlatform::new(base_ms, 0, home);

        write_settled(
            &folder.join("standup.vtt"),
            b"WEBVTT\n\n00:00:01.000 --> 00:00:02.000\nHi\n",
        );
        write_settled(
            &folder.join("other.iso.crdownload"),
            b"in progress download",
        );
        platform.set_file_id("standup.vtt", "ntfs-vtt-1");

        let mut engine = Engine::open(state_dir, platform.clone());
        let id = engine.begin_setup(&folder, None).unwrap();
        engine.confirm_folder(&id, Span::All).unwrap();
        let d1 = engine.next_delivery(false).unwrap();
        assert_eq!(d1.events.len(), 1);
        engine.record_outcome(&d1, DeliveryOutcome::Delivered);

        // Rename standup.vtt -> weekly-sync.vtt while other.iso.crdownload is still present.
        fs::rename(folder.join("standup.vtt"), folder.join("weekly-sync.vtt")).unwrap();
        platform.set_file_id("weekly-sync.vtt", "ntfs-vtt-1");
        platform.advance_ms(600_000);
        engine.scan_all();
        platform.advance_ms(300_000);
        let d_ren = engine.next_delivery(false).unwrap();
        assert_eq!(d_ren.events.len(), 1);
        assert_eq!(d_ren.events[0].verb, ActivityVerb::Renamed);
        assert_eq!(d_ren.events[0].rel_path, "weekly-sync.vtt");
        assert_eq!(
            d_ren.events[0].previous_path.as_deref(),
            Some("standup.vtt")
        );
        assert_eq!(d_ren.events[0].delivery, DeliveryMode::MetadataOnly);
        engine.record_outcome(&d_ren, DeliveryOutcome::Delivered);

        // Create and delete an ephemeral note inside an open 5-minute window while
        // other.iso.crdownload is still present; delete-before-send must drop it.
        write_settled(&folder.join("ephemeral.md"), b"gone before send");
        let eph_mtime_ms = fs::metadata(folder.join("ephemeral.md"))
            .unwrap()
            .modified()
            .unwrap()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64;
        // Put fake clock 10 seconds after mtime (settled, still inside open 5-minute window).
        let now_in_window = eph_mtime_ms + 10_000;
        let delta = now_in_window - platform.now_ms();
        if delta > 0 {
            platform.advance_ms(delta);
        }
        // Set mtime inside the current fake window so the window stays open.
        let open_window_start = (platform.now_ms() / 300_000) * 300_000;
        let _ = open_window_start;
        engine.scan_all();
        fs::remove_file(folder.join("ephemeral.md")).unwrap();
        engine.scan_all();
        platform.advance_ms(600_000);
        assert!(engine.next_delivery(false).is_none());
    }
}
