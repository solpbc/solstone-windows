// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! The vault engine against a real Windows file system: file attributes as a
//! directory listing reports them, and file ids across a rename. Windows only,
//! so the rest of the engine's tests stay on every host.

#![cfg(windows)]

use std::fs;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use solstone_vault::{Answer, Clock, Engine, ScanOutcome};

const T0: i64 = 1_794_155_400_000;

#[derive(Clone)]
struct TestClock(Arc<AtomicI64>);

impl Clock for TestClock {
    fn now_ms(&self) -> i64 {
        self.0.load(Ordering::SeqCst)
    }
    fn offset_s_at(&self, _ms: i64) -> i32 {
        -7 * 3600
    }
}

fn write_note(vault: &Path, name: &str, text: &str, mtime_ms: i64) {
    let path = vault.join(name);
    fs::write(&path, text).unwrap();
    fs::File::options()
        .write(true)
        .open(&path)
        .unwrap()
        .set_modified(std::time::UNIX_EPOCH + Duration::from_millis(mtime_ms as u64))
        .unwrap();
}

struct Fixture {
    temp: tempfile::TempDir,
    vault: std::path::PathBuf,
    clock: Arc<AtomicI64>,
    engine: Engine,
}

fn fixture() -> Fixture {
    let temp = tempfile::tempdir().unwrap();
    let vault = temp.path().join("Notes");
    fs::create_dir_all(vault.join(".obsidian")).unwrap();
    let clock = Arc::new(AtomicI64::new(T0));
    let engine = Engine::open(
        temp.path().join("state"),
        Arc::new(TestClock(Arc::clone(&clock))),
    )
    .unwrap()
    .with_dir_file_ids(Arc::new(platform_win::dir_file_ids::file_ids_in_dir));
    Fixture {
        temp,
        vault,
        clock,
        engine,
    }
}

fn turn_on(f: &mut Fixture) {
    f.engine.turn_on(&f.vault).unwrap();
    f.engine.apply_vault_list(&[]).unwrap();
}

fn scan(f: &mut Fixture) {
    let cancel = AtomicBool::new(false);
    assert_eq!(f.engine.scan(&cancel).unwrap(), ScanOutcome::Done);
}

/// Close everything due and deliver it, returning each delivered file's text.
fn deliver_all(f: &mut Fixture) -> Vec<String> {
    while f.engine.close_due_windows(500).unwrap() > 0 {}
    let mut files = Vec::new();
    while let Some(delivery) = f.engine.next_delivery() {
        files.push(fs::read_to_string(&delivery.path).unwrap());
        f.engine
            .record_answer(&delivery, Answer::Delivered)
            .unwrap();
    }
    files
}

fn mark_offline(path: &Path) {
    let status = std::process::Command::new("attrib")
        .arg("+O")
        .arg(path)
        .status()
        .unwrap();
    assert!(status.success(), "attrib +O: {status}");
}

#[test]
fn a_note_marked_offline_is_counted_not_read_and_never_deleted() {
    let mut f = fixture();
    write_note(&f.vault, "a.md", "alpha", T0 - 7_200_000);
    write_note(&f.vault, "b.md", "beta", T0 - 7_200_000);
    turn_on(&mut f);
    scan(&mut f);
    assert_eq!(f.engine.status(false, Some(true)).waiting, 2);
    assert_eq!(deliver_all(&mut f).len(), 1, "both notes share one window");

    mark_offline(&f.vault.join("a.md"));
    for _ in 0..3 {
        f.clock.fetch_add(10 * 60_000, Ordering::SeqCst);
        scan(&mut f);
    }
    let status = f.engine.status(false, Some(true));
    assert_eq!(status.not_on_device, 1);
    assert_eq!(
        status.waiting, 0,
        "an offline note is neither empty nor gone"
    );
    assert!(deliver_all(&mut f).is_empty());
}

/// The note contract's golden file, byte for byte, from real files on Windows.
/// Its move-with-an-edit can only pair by file id.
#[test]
fn the_engine_reproduces_the_contract_golden_file() {
    const GOLDEN: &str = concat!(
        r#"{"change":"changed","changed_at":"2026-11-08T09:42:10-07:00","content_hash":"f0b347123a86bae3e1d4d8c48a7f0430bed090b37d8ed1873a63a97c0b13dfa9","observed_at":"2026-11-08T09:42:33-07:00","path":"daily/2026-11-08.md","previous_path":null,"text":"---\ntags: [walk]\n---\nMet Alex by the river.\n","v":1,"vault":"Notes"}"#,
        "\n",
        r#"{"change":"removed","changed_at":"2026-11-08T09:44:33-07:00","content_hash":null,"observed_at":"2026-11-08T09:44:33-07:00","path":"inbox/old idea.md","previous_path":null,"text":null,"v":1,"vault":"Notes"}"#,
        "\n",
        r#"{"change":"renamed","changed_at":"2026-11-08T09:43:02-07:00","content_hash":"14fece546a747b07dec10e2658310b2d224447158a8c6e1b3a65d467b9de22a3","observed_at":"2026-11-08T09:43:33-07:00","path":"people/Alex Rivera.md","previous_path":"inbox/Alex.md","text":"Alex likes rivers.\n","v":1,"vault":"Notes"}"#,
        "\n",
    );
    let at = |offset_ms: i64| T0 + offset_ms;
    let mut f = fixture();
    for dir in ["inbox", "daily", "people"] {
        fs::create_dir_all(f.vault.join(dir)).unwrap();
    }
    write_note(&f.vault, "inbox/old idea.md", "an old idea\n", at(-600_000));
    write_note(&f.vault, "inbox/Alex.md", "Alex.\n", at(-600_000));
    turn_on(&mut f);
    scan(&mut f);
    f.clock.store(at(6 * 60_000), Ordering::SeqCst);
    assert_eq!(deliver_all(&mut f).len(), 1);

    f.clock.store(at(12 * 60_000 + 33_000), Ordering::SeqCst);
    write_note(
        &f.vault,
        "daily/2026-11-08.md",
        "---\ntags: [walk]\n---\nMet Alex by the river.\n",
        at(12 * 60_000 + 10_000),
    );
    scan(&mut f);

    f.clock.store(at(13 * 60_000 + 33_000), Ordering::SeqCst);
    fs::rename(
        f.vault.join("inbox/Alex.md"),
        f.vault.join("people/Alex Rivera.md"),
    )
    .unwrap();
    write_note(
        &f.vault,
        "people/Alex Rivera.md",
        "Alex likes rivers.\n",
        at(13 * 60_000 + 2_000),
    );
    scan(&mut f);

    f.clock.store(at(14 * 60_000 + 33_000), Ordering::SeqCst);
    fs::remove_file(f.vault.join("inbox/old idea.md")).unwrap();
    scan(&mut f);
    f.clock.store(at(15 * 60_000 + 33_000), Ordering::SeqCst);
    scan(&mut f);

    f.clock.store(at(16 * 60_000 + 30_000), Ordering::SeqCst);
    while f.engine.close_due_windows(500).unwrap() > 0 {}
    let delivery = f.engine.next_delivery().expect("the golden window");
    assert_eq!(delivery.window.day, "20261108");
    assert_eq!(delivery.window.segment, "094000_300");
    assert_eq!(
        delivery.name,
        "obsidian_3e100d06fb18b4f45302cb3cf2cad1881e7183360649f0543ce4acf565b9f81e.jsonl"
    );
    assert_eq!(fs::read_to_string(&delivery.path).unwrap(), GOLDEN);
}

#[test]
fn opening_a_note_that_became_a_placeholder_is_refused() {
    let f = fixture();
    write_note(&f.vault, "a.md", "alpha", T0 - 7_200_000);
    let path = f.vault.join("a.md");
    assert!(solstone_vault::platform::open_for_read(&path).is_ok());
    mark_offline(&path);
    assert!(solstone_vault::platform::open_for_read(&path).is_err());
}

#[test]
fn an_edit_and_a_rename_in_one_scan_pair_by_file_id() {
    let mut f = fixture();
    write_note(&f.vault, "a.md", "the words before", T0 - 7_200_000);
    turn_on(&mut f);
    scan(&mut f);
    assert_eq!(deliver_all(&mut f).len(), 1);

    // Content can't pair this: the text changes in the same scan as the name.
    f.clock.fetch_add(10 * 60_000, Ordering::SeqCst);
    fs::rename(f.vault.join("a.md"), f.vault.join("b.md")).unwrap();
    write_note(&f.vault, "b.md", "the words after", T0 + 5 * 60_000);
    scan(&mut f);
    f.clock.fetch_add(10 * 60_000, Ordering::SeqCst);
    scan(&mut f);

    let files = deliver_all(&mut f);
    assert_eq!(files.len(), 1, "{files:?}");
    let line: serde_json::Value = serde_json::from_str(files[0].trim_end()).unwrap();
    assert_eq!(line["change"], "renamed", "{line}");
    assert_eq!(line["previous_path"], "a.md");
    assert_eq!(line["path"], "b.md");
    assert_eq!(line["text"], "the words after");
}

#[test]
fn a_note_that_grew_too_large_and_moved_is_not_deleted() {
    let mut f = fixture();
    write_note(&f.vault, "a.md", "small", T0 - 7_200_000);
    turn_on(&mut f);
    scan(&mut f);
    assert_eq!(deliver_all(&mut f).len(), 1);

    fs::rename(f.vault.join("a.md"), f.vault.join("b.md")).unwrap();
    fs::write(f.vault.join("b.md"), vec![b'x'; 4 * 1024 * 1024 + 1]).unwrap();
    for _ in 0..3 {
        f.clock.fetch_add(10 * 60_000, Ordering::SeqCst);
        scan(&mut f);
    }
    let status = f.engine.status(false, Some(true));
    assert_eq!(status.too_large, 1);
    assert_eq!(status.waiting, 0, "no removal was reported");
}

#[test]
fn the_pending_log_truncates_on_windows_and_keeps_working() {
    let mut f = fixture();
    write_note(&f.vault, "a.md", "alpha", T0 - 7_200_000);
    turn_on(&mut f);
    scan(&mut f);
    let log = f.temp.path().join("state").join("pending.log");
    let clean_len = fs::metadata(&log).unwrap().len();
    assert!(clean_len > 0);

    // A torn tail is cut back on open, through its own write handle.
    use std::io::Write;
    fs::OpenOptions::new()
        .append(true)
        .open(&log)
        .unwrap()
        .write_all(b"{\"torn")
        .unwrap();
    let clock = Arc::clone(&f.clock);
    f.engine = Engine::open(f.temp.path().join("state"), Arc::new(TestClock(clock)))
        .unwrap()
        .with_dir_file_ids(Arc::new(platform_win::dir_file_ids::file_ids_in_dir));
    assert_eq!(fs::metadata(&log).unwrap().len(), clean_len);
    assert_eq!(f.engine.status(false, Some(true)).waiting, 1);

    // Draining empties the log the same way, and the log still takes new records.
    assert_eq!(deliver_all(&mut f).len(), 1);
    assert_eq!(fs::metadata(&log).unwrap().len(), 0);
    write_note(&f.vault, "b.md", "beta", T0 - 3_600_000);
    f.clock.fetch_add(10 * 60_000, Ordering::SeqCst);
    scan(&mut f);
    let files = deliver_all(&mut f);
    assert_eq!(files.len(), 1, "{files:?}");
    let line: serde_json::Value = serde_json::from_str(files[0].trim_end()).unwrap();
    assert_eq!(line["path"], "b.md");
    assert_eq!(line["text"], "beta");
}

#[test]
fn compaction_renames_over_the_open_pending_log() {
    use solstone_vault::pending::{Meta, Pending};
    use solstone_vault::{Change, Stamp, Window};

    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("pending.log");
    let stamp = Stamp::new(T0, -7 * 3600);
    let window: Window = stamp.window();
    let meta = |name: &str| Meta {
        change: Change::Changed,
        vault: "Notes".into(),
        path: name.into(),
        previous_path: None,
        changed_at: stamp,
        observed_at: stamp,
        content_hash: Some("x".repeat(64)),
        first_sight: false,
    };
    let mut pending = Pending::open(&path).unwrap();
    pending
        .put(&[], Some((&window, &meta("keep.md"), Some("kept"))))
        .unwrap();
    let body = "y".repeat(4 * 1024 * 1024);
    let mut dropped = Vec::new();
    for index in 0..20 {
        let name = format!("{index}.md");
        pending
            .put(&[], Some((&window, &meta(&name), Some(&body))))
            .unwrap();
        dropped.push((window.clone(), "Notes".to_owned(), name));
    }
    let before = fs::metadata(&path).unwrap().len();
    pending.remove_keys(&dropped).unwrap();
    assert!(
        fs::metadata(&path).unwrap().len() < before / 2,
        "the log was rewritten smaller"
    );
    let key = (window.clone(), "Notes".to_owned(), "keep.md".to_owned());
    assert_eq!(
        pending.read_line(&key).unwrap().0.text.as_deref(),
        Some("kept")
    );
    // The rewritten log is the one still open for writing.
    pending
        .put(&[], Some((&window, &meta("after.md"), Some("later"))))
        .unwrap();
    let after = (window, "Notes".to_owned(), "after.md".to_owned());
    assert_eq!(
        pending.read_line(&after).unwrap().0.text.as_deref(),
        Some("later")
    );
}
