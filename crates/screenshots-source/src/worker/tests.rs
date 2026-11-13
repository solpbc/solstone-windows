// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

use std::collections::{BTreeMap, VecDeque};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicI64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::sync::oneshot;

use super::*;
use crate::engine::{ConvertError, Engine, ListError, Listed, Platform};
use crate::ledger::{Ledger, Outcome};
use crate::rules::{Span, TakenFrom};

const OWN: &str = r"C:\Users\ana\Pictures\Screenshots";
const CHOSEN: &str = r"C:\Users\ana\Desktop";
const HOME: &str = r"C:\Users\ana";
/// 2026-11-13 10:00 local, at UTC-7.
const NOW: i64 = (20_770 * 86_400 + 17 * 3600) * 1000;
const MINUTE: i64 = 60_000;
const DAY: i64 = 86_400_000;

#[derive(Clone, Default)]
struct File {
    bytes: Vec<u8>,
    modified_ms: i64,
    created_ms: Option<i64>,
    hidden: bool,
    system: bool,
    cloud_only: bool,
    folder: bool,
    unreadable: bool,
}

#[derive(Default)]
struct Fs {
    /// (folder, name) -> file
    files: Mutex<BTreeMap<(String, String), File>>,
    now_ms: AtomicI64,
    reads: Mutex<Vec<PathBuf>>,
    list_error: Mutex<Option<ListError>>,
    own: Mutex<Option<String>>,
}

impl Fs {
    fn put(&self, folder: &str, name: &str, file: File) {
        self.files
            .lock()
            .unwrap()
            .insert((folder.to_string(), name.to_string()), file);
    }

    fn remove(&self, folder: &str, name: &str) {
        self.files
            .lock()
            .unwrap()
            .remove(&(folder.to_string(), name.to_string()));
    }

    fn reads_of(&self, name: &str) -> usize {
        self.reads
            .lock()
            .unwrap()
            .iter()
            .filter(|p| p.to_str().unwrap().ends_with(name))
            .count()
    }

    fn advance(&self, ms: i64) {
        self.now_ms.fetch_add(ms, Ordering::SeqCst);
    }

    fn now(&self) -> i64 {
        self.now_ms.load(Ordering::SeqCst)
    }
}

impl Platform for Arc<Fs> {
    fn now_ms(&self) -> i64 {
        self.now()
    }
    fn offset_at(&self, _secs: i64) -> Option<i32> {
        Some(-7 * 3600)
    }
    fn own_folder(&self) -> Option<PathBuf> {
        self.own.lock().unwrap().clone().map(PathBuf::from)
    }
    fn home(&self) -> Option<PathBuf> {
        Some(PathBuf::from(HOME))
    }
    fn list(&self, dir: &Path) -> Result<Vec<Listed>, ListError> {
        if let Some(error) = *self.list_error.lock().unwrap() {
            return Err(error);
        }
        let dir = dir.to_str().unwrap();
        let files = self.files.lock().unwrap();
        let listed: Vec<Listed> = files
            .iter()
            .filter(|((folder, _), _)| folder == dir)
            .map(|((folder, name), file)| Listed {
                path: PathBuf::from(format!("{folder}\\{name}")),
                name: name.clone(),
                size: file.bytes.len() as u64,
                modified_ms: file.modified_ms,
                created_ms: file.created_ms,
                regular: !file.folder,
                hidden: file.hidden,
                system: file.system,
                cloud_only: file.cloud_only,
            })
            .collect();
        if listed.is_empty() && !files.keys().any(|(folder, _)| folder == dir) && dir != OWN {
            return Err(ListError::Missing);
        }
        Ok(listed)
    }
    fn read(&self, path: &Path) -> io::Result<Vec<u8>> {
        self.reads.lock().unwrap().push(path.to_path_buf());
        let text = path.to_str().unwrap();
        let (folder, name) = text.rsplit_once('\\').unwrap();
        let files = self.files.lock().unwrap();
        match files.get(&(folder.to_string(), name.to_string())) {
            Some(file) if file.unreadable => Err(io::Error::from(io::ErrorKind::PermissionDenied)),
            Some(file) => Ok(file.bytes.clone()),
            None => Err(io::Error::from(io::ErrorKind::NotFound)),
        }
    }
    fn to_png(&self, bytes: &[u8]) -> Result<Vec<u8>, ConvertError> {
        if bytes.starts_with(b"BAD") {
            return Err(ConvertError::Undecodable);
        }
        if bytes.starts_with(b"BUSY") {
            return Err(ConvertError::Unavailable);
        }
        let (w, h) = if bytes.starts_with(b"HUGE") {
            (20_000, 20_000)
        } else {
            (4, 2)
        };
        Ok(png_sized(w, h, bytes))
    }
}

fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    out.extend_from_slice(kind);
    out.extend_from_slice(data);
    out.extend_from_slice(&[0; 4]);
}

fn png_sized(w: u32, h: u32, tag: &[u8]) -> Vec<u8> {
    let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut ihdr = w.to_be_bytes().to_vec();
    ihdr.extend_from_slice(&h.to_be_bytes());
    ihdr.extend_from_slice(&[8, 6, 0, 0, 0]);
    chunk(&mut out, b"IHDR", &ihdr);
    chunk(&mut out, b"tEXt", tag);
    chunk(&mut out, b"IEND", b"");
    out
}

fn png(tag: &str) -> Vec<u8> {
    png_sized(4, 2, tag.as_bytes())
}

fn game_bar_png(tag: &str) -> Vec<u8> {
    let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
    chunk(&mut out, b"IHDR", &[0, 0, 0, 4, 0, 0, 0, 2, 8, 6, 0, 0, 0]);
    chunk(
        &mut out,
        b"iTXt",
        b"Microsoft.GameDVR.Title\0\0\0\0\0Desktop",
    );
    chunk(&mut out, b"tEXt", tag.as_bytes());
    chunk(&mut out, b"IEND", b"");
    out
}

/// A screenshot taken `ago` ms before NOW, in Windows' own folder.
fn shot(f: &Fixture, name: &str, ago: i64) {
    shot_in(f, OWN, name, png(name), ago);
}

fn shot_in(f: &Fixture, folder: &str, name: &str, bytes: Vec<u8>, ago: i64) {
    let at = f.fs.now() - ago;
    f.fs.put(
        folder,
        name,
        File {
            bytes,
            modified_ms: at,
            created_ms: Some(at),
            ..File::default()
        },
    );
}

struct FakeJournal {
    capability: Mutex<Capability>,
    answers: Mutex<VecDeque<Delivery>>,
    delivered: Mutex<Vec<Upload>>,
    capability_calls: AtomicUsize,
    during_send: Mutex<Option<ScreenshotsHandle>>,
}

impl FakeJournal {
    fn names(&self) -> Vec<String> {
        self.delivered
            .lock()
            .unwrap()
            .iter()
            .map(|u| u.file_hash.clone())
            .collect()
    }
    fn count(&self) -> usize {
        self.delivered.lock().unwrap().len()
    }
}

impl Journal for Arc<FakeJournal> {
    fn capability(&self) -> BoxFuture<'_, Capability> {
        Box::pin(async move {
            self.capability_calls.fetch_add(1, Ordering::SeqCst);
            *self.capability.lock().unwrap()
        })
    }
    fn deliver<'a>(&'a self, upload: &'a Upload) -> BoxFuture<'a, Delivery> {
        Box::pin(async move {
            if let Some(handle) = self.during_send.lock().unwrap().take() {
                handle.restart_setup();
            }
            self.delivered.lock().unwrap().push(upload.clone());
            self.answers
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or(Delivery::Answered {
                    http_status: 200,
                    status: Some("ok".into()),
                    reason_code: None,
                })
        })
    }
    fn pairing_unchanged(&self) -> BoxFuture<'_, bool> {
        Box::pin(async move { true })
    }
}

struct Fixture {
    fs: Arc<Fs>,
    journal: Arc<FakeJournal>,
    worker: Worker<Arc<FakeJournal>>,
    handle: ScreenshotsHandle,
    paused: Arc<Mutex<Option<Pause>>>,
    counters: source_counters::SharedCounters,
    dir: PathBuf,
    _temp: tempfile::TempDir,
}

fn fixture() -> Fixture {
    let temp = tempfile::tempdir().unwrap();
    let dir = temp.path().join("screenshots");
    fixture_in(temp, dir, None)
}

fn fixture_in(temp: tempfile::TempDir, dir: PathBuf, fs: Option<Arc<Fs>>) -> Fixture {
    let fs = fs.unwrap_or_else(|| {
        let fs = Arc::new(Fs::default());
        fs.now_ms.store(NOW, Ordering::SeqCst);
        *fs.own.lock().unwrap() = Some(OWN.to_string());
        fs
    });
    let journal = Arc::new(FakeJournal {
        capability: Mutex::new(Capability::Supported),
        answers: Mutex::new(VecDeque::new()),
        delivered: Mutex::new(Vec::new()),
        capability_calls: AtomicUsize::new(0),
        during_send: Mutex::new(None),
    });
    let paused: Arc<Mutex<Option<Pause>>> = Arc::new(Mutex::new(None));
    let read = Arc::clone(&paused);
    let counters = source_counters::SharedCounters::open(temp.path(), "2026-11-13").unwrap();
    let (handle, mut worker) = Worker::new(
        Arc::clone(&journal),
        Arc::new(move || *read.lock().unwrap()),
    );
    let platform: Arc<dyn Platform> = Arc::new(Arc::clone(&fs));
    worker.set_engine(Engine::open(dir.clone(), platform, Some(counters.clone())).unwrap());
    worker.publish();
    Fixture {
        fs,
        journal,
        worker,
        handle,
        paused,
        counters,
        dir,
        _temp: temp,
    }
}

/// Quit and launch again over the same ledger and folder.
fn relaunch(f: Fixture) -> Fixture {
    let Fixture { fs, dir, _temp, .. } = f;
    fixture_in(_temp, dir, Some(fs))
}

async fn call(
    f: &mut Fixture,
    build: impl FnOnce(oneshot::Sender<ScreenshotsDocument>) -> Command,
) -> Status {
    let (reply, answer) = oneshot::channel();
    f.worker.handle(build(reply)).await;
    answer.await.unwrap().status
}

async fn turn_on(f: &mut Fixture) -> Status {
    call(f, Command::TurnOn).await
}

async fn confirm(f: &mut Fixture, span: Span) -> Status {
    call(f, |reply| Command::Confirm(span, reply)).await
}

/// Look at the folder now and send until nothing more goes.
async fn look(f: &mut Fixture) {
    f.worker.next_scan = Instant::now();
    for _ in 0..50 {
        if !f.worker.step().await {
            break;
        }
    }
    f.worker.publish();
}

fn status(f: &Fixture) -> Status {
    f.handle.status().unwrap()
}

fn outcomes(f: &Fixture) -> Vec<Outcome> {
    f.worker
        .engine
        .as_ref()
        .unwrap()
        .ledger()
        .items
        .iter()
        .map(|i| i.outcome)
        .collect()
}

fn count_for(s: &Status, span: Span) -> u32 {
    s.counts.as_ref().map_or(0, |c| c[&span])
}

// ── states ──────────────────────────────────────────────────────────────────

#[tokio::test]
async fn nothing_is_listed_until_the_owner_turns_it_on() {
    let mut f = fixture();
    shot(&f, "Screenshot (1).png", DAY);
    look(&mut f).await;
    assert_eq!(status(&f).state, ScreenshotsState::Off);
    assert_eq!(
        f.fs.reads_of("Screenshot (1).png"),
        0,
        "nothing read while off"
    );
    assert!(status(&f).counts.is_none());

    let s = turn_on(&mut f).await;
    assert_eq!(s.state, ScreenshotsState::Choosing);
    assert_eq!(count_for(&s, Span::ThirtyDays), 1);
    assert!(s.windows_folder);
    assert_eq!(
        s.folder_line.as_deref(),
        Some("Pictures \u{203A} Screenshots")
    );
}

#[tokio::test]
async fn no_folder_resolving_waits_for_one_to_be_chosen() {
    let mut f = fixture();
    *f.fs.own.lock().unwrap() = None;
    f.worker.publish();
    assert_eq!(status(&f).state, ScreenshotsState::NoFolder);
    shot_in(
        &f,
        CHOSEN,
        "Screenshot 2026-11-12 090000.png",
        png("a"),
        DAY,
    );
    let s = call(&mut f, |r| Command::ChooseFolder(CHOSEN.into(), r)).await;
    assert_eq!(s.state, ScreenshotsState::Choosing);
    assert!(!s.windows_folder);
    assert_eq!(count_for(&s, Span::ThirtyDays), 1);
}

#[tokio::test]
async fn a_count_that_failed_waits_to_be_set_up() {
    let mut f = fixture();
    *f.fs.list_error.lock().unwrap() = Some(ListError::Unreadable);
    let s = turn_on(&mut f).await;
    assert_eq!(s.state, ScreenshotsState::Choosing, "not a fault");
    assert!(s.count_failed);
    assert!(!confirm(&mut f, Span::ThirtyDays)
        .await
        .state
        .eq(&ScreenshotsState::On));

    *f.fs.list_error.lock().unwrap() = None;
    let s = call(&mut f, Command::TryAgain).await;
    assert!(!s.count_failed);
    assert!(s.counts.is_some());
}

#[tokio::test]
async fn windows_folder_not_made_yet_reads_empty_and_a_folder_that_went_away_needs_attention() {
    let mut f = fixture();
    *f.fs.own.lock().unwrap() = Some(r"C:\Users\ana\Pictures\Shots".into());
    let s = turn_on(&mut f).await;
    assert_eq!(
        s.state,
        ScreenshotsState::Choosing,
        "a folder Windows hasn't made yet"
    );
    assert_eq!(count_for(&s, Span::All), 0);

    let mut f = fixture();
    shot_in(&f, CHOSEN, "Screenshot (2).png", png("x"), DAY);
    call(&mut f, |r| Command::ChooseFolder(CHOSEN.into(), r)).await;
    confirm(&mut f, Span::ThirtyDays).await;
    look(&mut f).await;
    f.fs.remove(CHOSEN, "Screenshot (2).png");
    look(&mut f).await;
    assert_eq!(status(&f).state, ScreenshotsState::FolderMissing);
    assert_eq!(status(&f).folder_name.as_deref(), Some("Desktop"));
}

// ── what a folder gives up ──────────────────────────────────────────────────

#[tokio::test]
async fn windows_own_folder_takes_every_picture_and_a_chosen_folder_only_what_windows_named() {
    let mut f = fixture();
    for folder in [OWN, CHOSEN] {
        shot_in(
            &f,
            folder,
            "Screenshot (3).png",
            png(&format!("{folder}1")),
            DAY,
        );
        shot_in(
            &f,
            folder,
            "Screenshot 2026-11-12 140000.jpg",
            png(&format!("{folder}2")),
            DAY,
        );
        shot_in(
            &f,
            folder,
            "Desktop 2026.11.12 - 14.00.00.01.png",
            game_bar_png(folder),
            DAY,
        );
        shot_in(&f, folder, "holiday.png", png(&format!("{folder}3")), DAY);
        shot_in(&f, folder, "IMG_0042.jpg", png(&format!("{folder}4")), DAY);
    }
    let s = turn_on(&mut f).await;
    assert_eq!(
        count_for(&s, Span::All),
        5,
        "every picture in Windows' folder"
    );

    let s = call(&mut f, |r| Command::ChooseFolder(CHOSEN.into(), r)).await;
    assert_eq!(count_for(&s, Span::All), 3, "the named and the marked only");
    assert_eq!(
        f.fs.reads_of(r"Desktop\IMG_0042.jpg"),
        0,
        "an unnamed JPEG isn't even read"
    );

    // Choosing Windows' own folder is still Windows' own folder.
    let s = call(&mut f, |r| Command::ChooseFolder(OWN.into(), r)).await;
    assert!(s.windows_folder);
    assert_eq!(count_for(&s, Span::All), 5);
}

#[tokio::test]
async fn what_is_never_a_screenshot_stays_out() {
    let mut f = fixture();
    shot(&f, "real.png", DAY);
    shot_in(&f, OWN, "clip.mp4", b"video".to_vec(), DAY);
    shot_in(&f, OWN, "notes.txt", b"text".to_vec(), DAY);
    shot_in(&f, OWN, "desktop.ini", b"[x]".to_vec(), DAY);
    shot_in(&f, OWN, "a.png.crdownload", b"x".to_vec(), DAY);
    shot_in(&f, OWN, "fresh.png", png("fresh"), 1_000);
    let hidden = File {
        bytes: png("h"),
        modified_ms: NOW - DAY,
        hidden: true,
        ..File::default()
    };
    f.fs.put(OWN, "hidden.png", hidden);
    let system = File {
        bytes: png("s"),
        modified_ms: NOW - DAY,
        system: true,
        ..File::default()
    };
    f.fs.put(OWN, "system.png", system);
    let folder = File {
        modified_ms: NOW - DAY,
        folder: true,
        ..File::default()
    };
    f.fs.put(OWN, "inner.png", folder);
    let s = turn_on(&mut f).await;
    assert_eq!(count_for(&s, Span::All), 1);

    // A file still being written comes in once it has settled.
    f.fs.advance(3_000);
    let s = call(&mut f, Command::TryAgain).await;
    assert_eq!(count_for(&s, Span::All), 2);
}

#[tokio::test]
async fn the_capture_time_comes_from_the_name_then_the_creation_then_the_change() {
    let mut f = fixture();
    // Named 09:15:30 local on the 12th, created later.
    shot_in(
        &f,
        OWN,
        "Screenshot 2026-11-12 091530.png",
        png("n"),
        MINUTE,
    );
    shot_in(&f, OWN, "made.png", png("c"), 3 * MINUTE);
    f.fs.put(
        OWN,
        "changed.png",
        File {
            bytes: png("m"),
            modified_ms: NOW - 5 * MINUTE,
            created_ms: None,
            ..File::default()
        },
    );
    turn_on(&mut f).await;
    confirm(&mut f, Span::ThirtyDays).await;
    look(&mut f).await;
    let delivered = f.journal.delivered.lock().unwrap().clone();
    let by = |at: &str| delivered.iter().find(|u| u.captured_at == at).cloned();
    let named = by("2026-11-12T09:15:30-07:00").expect("the name's own time");
    assert_eq!(named.taken_from, TakenFrom::Platform);
    assert_eq!(
        (named.day.as_str(), named.segment.as_str()),
        ("20261112", "091530_1")
    );
    assert_eq!(
        by("2026-11-13T09:57:00-07:00").unwrap().taken_from,
        TakenFrom::Platform
    );
    assert_eq!(
        by("2026-11-13T09:55:00-07:00").unwrap().taken_from,
        TakenFrom::File
    );
}

// ── the confirm ─────────────────────────────────────────────────────────────

#[tokio::test]
async fn confirm_freezes_the_count_and_sends_each_hash_once() {
    let mut f = fixture();
    shot(&f, "a.png", DAY);
    shot(&f, "b.png", 2 * DAY);
    shot_in(&f, OWN, "a copy.png", png("a.png"), DAY);
    let s = turn_on(&mut f).await;
    assert_eq!(
        count_for(&s, Span::ThirtyDays),
        2,
        "the copy is the same screenshot"
    );
    let s = confirm(&mut f, Span::ThirtyDays).await;
    assert_eq!(s.state, ScreenshotsState::BringingIn);
    assert_eq!(s.backlog.unwrap().total, 2);
    look(&mut f).await;
    assert_eq!(f.journal.count(), 2);
    let s = status(&f);
    assert_eq!(s.state, ScreenshotsState::On);
    assert!(s.backlog.is_none());
    look(&mut f).await;
    assert_eq!(f.journal.count(), 2, "never twice");
    assert_eq!(f.counters.screenshot_count(), 2);

    let upload = &f.journal.delivered.lock().unwrap()[0];
    assert_eq!(upload.filename, "screenshot_screen.png");
    assert_eq!(upload.content_type, "image/png");
}

#[tokio::test]
async fn screenshots_from_before_the_chosen_span_stay_out_after_the_count() {
    let mut f = fixture();
    shot(&f, "recent.png", 2 * DAY);
    shot(&f, "old.png", 60 * DAY);
    turn_on(&mut f).await;
    confirm(&mut f, Span::ThirtyDays).await;
    look(&mut f).await;
    look(&mut f).await;
    assert_eq!(
        f.journal.count(),
        1,
        "the older one never becomes a new item"
    );
    assert_eq!(f.counters.screenshot_count(), 1);
}

#[tokio::test]
async fn nothing_from_before_today_starts_from_today() {
    let mut f = fixture();
    shot(&f, "yesterday.png", DAY);
    shot(&f, "this morning.png", 2 * 3_600_000);
    let s = turn_on(&mut f).await;
    assert_eq!(count_for(&s, Span::Nothing), 1);
    confirm(&mut f, Span::Nothing).await;
    look(&mut f).await;
    assert_eq!(f.journal.count(), 1);
    shot(&f, "now.png", 3_000);
    look(&mut f).await;
    assert_eq!(f.journal.count(), 2, "and each new one after");
}

#[tokio::test]
async fn a_second_backfill_counts_only_its_own_screenshots() {
    let mut f = fixture();
    shot(&f, "a.png", DAY);
    shot(&f, "b.png", DAY);
    turn_on(&mut f).await;
    confirm(&mut f, Span::ThirtyDays).await;
    look(&mut f).await;
    call(&mut f, Command::TurnOff).await;

    shot(&f, "c.png", 100 * DAY);
    let s = turn_on(&mut f).await;
    assert!(s.turned_back_on, "what's in already isn't brought in again");
    assert_eq!(count_for(&s, Span::Year), 1, "a and b are in");
    let s = confirm(&mut f, Span::Year).await;
    let backlog = s.backlog.unwrap();
    assert_eq!((backlog.done, backlog.total), (0, 1));
    look(&mut f).await;
    assert_eq!(f.journal.count(), 3);
}

#[tokio::test]
async fn turning_off_clears_the_pause_so_a_new_span_can_bring_it() {
    let mut f = fixture();
    turn_on(&mut f).await;
    confirm(&mut f, Span::Nothing).await;
    *f.paused.lock().unwrap() = Some(Pause { until_secs: None });
    look(&mut f).await;
    f.fs.advance(10 * MINUTE);
    shot(&f, "while paused.png", 5 * MINUTE);
    look(&mut f).await;
    assert_eq!(f.journal.count(), 0);
    *f.paused.lock().unwrap() = None;
    look(&mut f).await;
    assert_eq!(f.journal.count(), 0, "resuming doesn't catch it");

    call(&mut f, Command::TurnOff).await;
    turn_on(&mut f).await;
    confirm(&mut f, Span::ThirtyDays).await;
    look(&mut f).await;
    assert_eq!(f.journal.count(), 1, "a new span is the owner's choice");
}

// ── pause ───────────────────────────────────────────────────────────────────

#[tokio::test]
async fn pause_skips_the_window_and_resume_does_not_catch_it() {
    let mut f = fixture();
    turn_on(&mut f).await;
    confirm(&mut f, Span::Nothing).await;
    look(&mut f).await;
    let paused_at = f.fs.now();
    *f.paused.lock().unwrap() = Some(Pause { until_secs: None });
    look(&mut f).await;
    assert_eq!(status(&f).state, ScreenshotsState::Paused);
    f.fs.advance(10 * MINUTE);
    *f.paused.lock().unwrap() = None;
    look(&mut f).await;
    let resumed_at = f.fs.now();
    f.fs.advance(5_000);
    // Taken the moment the pause began, inside it, and the moment of resume.
    shot_in(&f, OWN, "at pause.png", png("p0"), f.fs.now() - paused_at);
    shot_in(
        &f,
        OWN,
        "during.png",
        png("p1"),
        f.fs.now() - paused_at - MINUTE,
    );
    shot_in(&f, OWN, "at resume.png", png("p2"), f.fs.now() - resumed_at);
    look(&mut f).await;
    let delivered = f.journal.names();
    assert_eq!(delivered.len(), 1, "only the moment of resume comes in");
    assert_eq!(status(&f).state, ScreenshotsState::On);
}

#[tokio::test]
async fn a_pause_that_ran_out_ends_when_it_was_due() {
    let mut f = fixture();
    turn_on(&mut f).await;
    confirm(&mut f, Span::Nothing).await;
    let until = f.fs.now() / 1000 + 300;
    *f.paused.lock().unwrap() = Some(Pause {
        until_secs: Some(until),
    });
    look(&mut f).await;
    // The app was away and came back long after the pause ran out.
    f.fs.advance(60 * MINUTE);
    *f.paused.lock().unwrap() = None;
    shot(&f, "after it ran out.png", 30 * MINUTE);
    look(&mut f).await;
    assert_eq!(f.journal.count(), 1);
}

#[tokio::test]
async fn what_was_chosen_from_before_still_goes_while_paused() {
    let mut f = fixture();
    shot(&f, "old.png", DAY);
    turn_on(&mut f).await;
    confirm(&mut f, Span::ThirtyDays).await;
    *f.paused.lock().unwrap() = Some(Pause { until_secs: None });
    look(&mut f).await;
    assert_eq!(f.journal.count(), 1);
}

// ── delivery ────────────────────────────────────────────────────────────────

#[tokio::test]
async fn a_convertible_type_is_sent_as_png_once() {
    let mut f = fixture();
    shot_in(&f, OWN, "shot.gif", b"GIF89a-one".to_vec(), DAY);
    shot_in(&f, OWN, "shot.bmp", b"BM-two".to_vec(), DAY);
    turn_on(&mut f).await;
    confirm(&mut f, Span::ThirtyDays).await;
    look(&mut f).await;
    look(&mut f).await;
    let delivered = f.journal.delivered.lock().unwrap().clone();
    assert_eq!(delivered.len(), 2);
    for upload in &delivered {
        assert_eq!(upload.filename, "screenshot_screen.png");
        assert_eq!(upload.content_type, "image/png");
        assert!(upload.bytes.starts_with(b"\x89PNG"));
    }
}

#[tokio::test]
async fn a_size_or_type_the_journal_will_not_keep_stays_unsent_and_is_final() {
    let mut f = fixture();
    let mut big = png("big");
    big.resize(crate::rules::MAX_BYTES as usize + 1, 0);
    shot_in(&f, OWN, "big.png", big, DAY);
    shot_in(&f, OWN, "huge.tif", b"HUGE".to_vec(), DAY);
    shot_in(&f, OWN, "broken.heic", b"BAD".to_vec(), DAY);
    shot(&f, "fine.png", DAY);
    turn_on(&mut f).await;
    confirm(&mut f, Span::ThirtyDays).await;
    look(&mut f).await;
    assert_eq!(f.journal.count(), 1, "only the one it keeps");
    let s = status(&f);
    assert_eq!(s.cant_come_in, 3);
    assert_eq!(s.state, ScreenshotsState::On, "the count still completes");
    look(&mut f).await;
    assert_eq!(f.journal.count(), 1, "final: not tried again");
}

#[tokio::test]
async fn a_converter_that_cannot_run_tries_again() {
    let mut f = fixture();
    shot_in(&f, OWN, "a.tiff", b"BUSY".to_vec(), DAY);
    turn_on(&mut f).await;
    confirm(&mut f, Span::ThirtyDays).await;
    look(&mut f).await;
    assert_eq!(status(&f).waiting, 1);
    assert_eq!(status(&f).cant_come_in, 0);
}

#[tokio::test]
async fn a_file_deleted_before_send_counts_and_never_goes() {
    let mut f = fixture();
    shot(&f, "a.png", DAY);
    shot(&f, "b.png", DAY);
    turn_on(&mut f).await;
    confirm(&mut f, Span::ThirtyDays).await;
    f.fs.remove(OWN, "a.png");
    look(&mut f).await;
    assert_eq!(f.journal.count(), 1);
    assert!(outcomes(&f).contains(&Outcome::Deleted));
    assert_eq!(
        status(&f).state,
        ScreenshotsState::On,
        "the deleted one counts as done"
    );
}

#[tokio::test]
async fn ahead_waits_and_invalid_is_final() {
    let mut f = fixture();
    shot(&f, "a.png", DAY);
    shot(&f, "b.png", 2 * DAY);
    turn_on(&mut f).await;
    confirm(&mut f, Span::ThirtyDays).await;
    f.journal.answers.lock().unwrap().extend([
        Delivery::Answered {
            http_status: 400,
            status: None,
            reason_code: Some("screenshot_invalid".into()),
        },
        Delivery::Answered {
            http_status: 409,
            status: None,
            reason_code: Some("screenshot_ahead".into()),
        },
    ]);
    look(&mut f).await;
    assert_eq!(f.journal.count(), 2, "the look stops at the one that waits");
    let s = status(&f);
    assert_eq!(s.cant_come_in, 1);
    assert_eq!(s.waiting, 1);
    look(&mut f).await;
    assert_eq!(f.journal.count(), 3, "the held one went again");
    assert_eq!(status(&f).waiting, 0);
}

#[tokio::test]
async fn at_most_eight_go_in_one_look() {
    let mut f = fixture();
    for n in 0..10 {
        shot(&f, &format!("{n}.png"), DAY + n);
    }
    turn_on(&mut f).await;
    confirm(&mut f, Span::ThirtyDays).await;
    f.worker.next_scan = Instant::now();
    assert!(f.worker.step().await, "a full look asks for another");
    assert_eq!(f.journal.count(), SENDS_PER_LOOK);
    assert!(!f.worker.step().await);
    assert_eq!(f.journal.count(), 10);
}

#[tokio::test]
async fn an_older_journal_holds_the_shots() {
    let mut f = fixture();
    shot(&f, "a.png", DAY);
    *f.journal.capability.lock().unwrap() = Capability::NotListed;
    turn_on(&mut f).await;
    confirm(&mut f, Span::ThirtyDays).await;
    look(&mut f).await;
    assert_eq!(f.journal.count(), 0);
    let s = status(&f);
    assert!(s.waiting_for_journal);
    assert_eq!(s.waiting, 1);
    assert_eq!(s.state, ScreenshotsState::BringingIn, "on, not a fault");
}

#[tokio::test]
async fn a_journal_that_updates_is_asked_again() {
    let mut f = fixture();
    shot(&f, "a.png", DAY);
    *f.journal.capability.lock().unwrap() = Capability::NotListed;
    turn_on(&mut f).await;
    confirm(&mut f, Span::ThirtyDays).await;
    look(&mut f).await;
    let asked = f.journal.capability_calls.load(Ordering::SeqCst);
    *f.journal.capability.lock().unwrap() = Capability::Supported;
    look(&mut f).await;
    assert_eq!(f.journal.count(), 0, "not before the minute is up");
    f.worker.capability_checked = Some(Instant::now() - CAPABILITY_RETRY - Duration::from_secs(1));
    look(&mut f).await;
    assert!(f.journal.capability_calls.load(Ordering::SeqCst) > asked);
    assert_eq!(f.journal.count(), 1);
    assert!(!status(&f).waiting_for_journal);
}

#[tokio::test]
async fn a_new_screenshot_that_waits_across_ticks_does_not_trap() {
    let mut f = fixture();
    turn_on(&mut f).await;
    confirm(&mut f, Span::Nothing).await;
    *f.journal.capability.lock().unwrap() = Capability::Unknown;
    shot(&f, "a.png", 5_000);
    for _ in 0..5 {
        look(&mut f).await;
    }
    assert_eq!(status(&f).waiting, 1, "still waiting, still there");
    *f.journal.capability.lock().unwrap() = Capability::Supported;
    f.worker.capability_checked = None;
    look(&mut f).await;
    assert_eq!(f.journal.count(), 1);
}

#[tokio::test]
async fn a_cloud_file_is_counted_from_its_listing_and_read_only_when_it_is_sent() {
    let mut f = fixture();
    f.fs.put(
        OWN,
        "Screenshot (9).png",
        File {
            bytes: png("cloud"),
            modified_ms: NOW - DAY,
            created_ms: Some(NOW - DAY),
            cloud_only: true,
            ..File::default()
        },
    );
    let s = turn_on(&mut f).await;
    assert_eq!(count_for(&s, Span::ThirtyDays), 1);
    assert_eq!(
        f.fs.reads_of("Screenshot (9).png"),
        0,
        "the count read nothing"
    );
    confirm(&mut f, Span::ThirtyDays).await;
    assert_eq!(f.counters.screenshot_count(), 0, "not hashed yet");
    look(&mut f).await;
    assert_eq!(f.journal.count(), 1);
    assert_eq!(f.counters.screenshot_count(), 1);
}

#[tokio::test]
async fn a_screenshot_that_cannot_be_read_is_read_again() {
    let mut f = fixture();
    f.fs.put(
        OWN,
        "locked.png",
        File {
            bytes: png("locked"),
            modified_ms: NOW - DAY,
            created_ms: Some(NOW - DAY),
            unreadable: true,
            ..File::default()
        },
    );
    turn_on(&mut f).await;
    confirm(&mut f, Span::ThirtyDays).await;
    look(&mut f).await;
    assert_eq!(status(&f).unreadable, 1);
    assert_eq!(f.journal.count(), 0);

    f.fs.files
        .lock()
        .unwrap()
        .get_mut(&(OWN.to_string(), "locked.png".to_string()))
        .unwrap()
        .unreadable = false;
    call(&mut f, Command::TryAgain).await;
    look(&mut f).await;
    assert_eq!(status(&f).unreadable, 0);
    assert_eq!(f.journal.count(), 1);
}

#[tokio::test]
async fn a_file_is_hashed_once_until_it_changes() {
    let mut f = fixture();
    shot(&f, "a.png", DAY);
    turn_on(&mut f).await;
    for _ in 0..3 {
        call(&mut f, Command::TryAgain).await;
    }
    assert_eq!(f.fs.reads_of("a.png"), 1);
    shot(&f, "a.png", 10_000);
    call(&mut f, Command::TryAgain).await;
    assert_eq!(f.fs.reads_of("a.png"), 2);
}

// ── a new pairing ───────────────────────────────────────────────────────────

#[tokio::test]
async fn a_new_pairing_asks_again_and_sends_nothing_until_the_count() {
    let mut f = fixture();
    shot(&f, "a.png", DAY);
    turn_on(&mut f).await;
    confirm(&mut f, Span::ThirtyDays).await;
    look(&mut f).await;
    assert_eq!(f.journal.count(), 1);

    f.handle.restart_setup();
    f.worker.restart_if_asked().await;
    look(&mut f).await;
    let s = status(&f);
    assert_eq!(s.state, ScreenshotsState::Choosing);
    assert!(!s.turned_back_on, "a new journal, not a turn back on");
    assert_eq!(
        count_for(&s, Span::ThirtyDays),
        1,
        "the new journal gets what the old one had"
    );
    assert_eq!(f.journal.count(), 1, "nothing before the count");

    confirm(&mut f, Span::ThirtyDays).await;
    look(&mut f).await;
    assert_eq!(f.journal.count(), 2, "sent again, to the new journal");
    assert_eq!(
        f.counters.screenshot_count(),
        1,
        "still one screenshot taken in"
    );
}

#[tokio::test]
async fn a_new_pairing_while_off_asks_again_when_turned_on() {
    let mut f = fixture();
    shot(&f, "a.png", DAY);
    turn_on(&mut f).await;
    confirm(&mut f, Span::ThirtyDays).await;
    look(&mut f).await;
    call(&mut f, Command::TurnOff).await;
    f.handle.restart_setup();
    f.worker.restart_if_asked().await;
    let s = turn_on(&mut f).await;
    assert_eq!(s.state, ScreenshotsState::Choosing);
    assert!(!s.turned_back_on);
    assert_eq!(count_for(&s, Span::ThirtyDays), 1);
}

#[tokio::test]
async fn a_screenshot_waiting_when_a_new_pairing_lands_still_goes() {
    let mut f = fixture();
    turn_on(&mut f).await;
    confirm(&mut f, Span::Nothing).await;
    *f.journal.capability.lock().unwrap() = Capability::Unknown;
    shot(&f, "waiting.png", 5_000);
    look(&mut f).await;
    assert_eq!(status(&f).waiting, 1);

    *f.journal.capability.lock().unwrap() = Capability::Supported;
    f.handle.restart_setup();
    f.worker.restart_if_asked().await;
    look(&mut f).await;
    assert_eq!(f.journal.count(), 0, "not before the count");
    confirm(&mut f, Span::Nothing).await;
    look(&mut f).await;
    assert_eq!(f.journal.count(), 1);
}

#[tokio::test]
async fn a_new_pairing_during_a_send_leaves_no_outcome_and_does_not_trap() {
    let mut f = fixture();
    shot(&f, "a.png", DAY);
    shot(&f, "b.png", DAY + 1);
    turn_on(&mut f).await;
    confirm(&mut f, Span::ThirtyDays).await;
    *f.journal.during_send.lock().unwrap() = Some(f.handle.clone());
    f.worker.next_scan = Instant::now();
    f.worker.step().await;
    assert_eq!(f.journal.count(), 1, "the look stopped at the new pairing");
    assert!(
        !outcomes(&f).contains(&Outcome::Delivered),
        "the old journal's answer records nothing"
    );
    f.worker.restart_if_asked().await;
    look(&mut f).await;
    assert_eq!(status(&f).state, ScreenshotsState::Choosing);
    confirm(&mut f, Span::ThirtyDays).await;
    look(&mut f).await;
    assert_eq!(f.journal.count(), 3, "both go to the new journal");
    assert_eq!(status(&f).state, ScreenshotsState::On);
}

// ── across launches ─────────────────────────────────────────────────────────

#[tokio::test]
async fn a_relaunch_starts_nothing_again_and_catches_up() {
    let mut f = fixture();
    shot(&f, "a.png", DAY);
    turn_on(&mut f).await;
    confirm(&mut f, Span::ThirtyDays).await;
    look(&mut f).await;
    assert_eq!(f.journal.count(), 1);

    // Quit, take one while the app is closed, launch again.
    let fs = Arc::clone(&f.fs);
    let mut f = relaunch(f);
    fs.advance(30 * MINUTE);
    shot(&f, "while closed.png", 10 * MINUTE);
    look(&mut f).await;
    assert_eq!(status(&f).state, ScreenshotsState::On, "no chooser again");
    assert_eq!(
        f.journal.count(),
        1,
        "only the new one, to this launch's journal"
    );
    assert_eq!(f.journal.names().len(), 1);
}

#[tokio::test]
async fn a_ledger_saved_before_newer_fields_still_loads() {
    let temp = tempfile::tempdir().unwrap();
    let dir = temp.path().join("screenshots");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join(crate::ledger::FILE_NAME),
        r#"{"v":1,"consented":true,"enabled":true,"confirmed":true,"items":[{"id":"ab","path":"C:\\x.png","outcome":"delivered"}]}"#,
    )
    .unwrap();
    let ledger = Ledger::load(&dir).unwrap();
    assert!(ledger.confirmed);
    assert_eq!(ledger.items[0].outcome, Outcome::Delivered);
    assert!(ledger.pause_windows.is_empty());
    let f = fixture_in(temp, dir, None);
    assert_eq!(status(&f).state, ScreenshotsState::On);
}

#[tokio::test]
async fn a_ledger_that_does_not_read_is_set_aside() {
    let temp = tempfile::tempdir().unwrap();
    let dir = temp.path().join("screenshots");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join(crate::ledger::FILE_NAME), b"not json").unwrap();
    let f = fixture_in(temp, dir.clone(), None);
    assert_eq!(status(&f).state, ScreenshotsState::Off);
    assert!(dir.join("ledger.json.unreadable").exists());
}

#[tokio::test]
async fn the_running_source_takes_a_new_pairing_without_a_command() {
    let mut f = fixture();
    shot(&f, "a.png", DAY);
    turn_on(&mut f).await;
    confirm(&mut f, Span::ThirtyDays).await;
    look(&mut f).await;
    let Fixture {
        worker,
        handle,
        _temp,
        ..
    } = f;
    let mut state = handle.state_changes();
    let task = tokio::spawn(worker.run());
    handle.restart_setup();
    tokio::time::timeout(Duration::from_secs(10), async {
        while *state.borrow_and_update() != Some(ScreenshotsState::Choosing) {
            state.changed().await.unwrap();
        }
    })
    .await
    .expect("back to the chooser");
    task.abort();
}

#[tokio::test]
async fn choosing_another_folder_counts_again() {
    let mut f = fixture();
    shot(&f, "a.png", DAY);
    turn_on(&mut f).await;
    confirm(&mut f, Span::ThirtyDays).await;
    look(&mut f).await;
    shot_in(&f, CHOSEN, "Screenshot (1).png", png("other"), DAY);
    let s = call(&mut f, |r| Command::ChooseFolder(CHOSEN.into(), r)).await;
    assert_eq!(s.state, ScreenshotsState::Choosing);
    assert_eq!(s.folder_name.as_deref(), Some("Desktop"));
    assert_eq!(count_for(&s, Span::ThirtyDays), 1);
}
