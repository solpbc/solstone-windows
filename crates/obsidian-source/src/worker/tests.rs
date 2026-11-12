// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! The source's rules through the real engine, with a fake journal at the seam.
//! The app's own test drives the real client against the journal's literal bodies.

use super::*;
use solstone_vault::{Attention, Clock};
use std::collections::VecDeque;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicI64;

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

/// The fake's name for itself: it must never reach the source's files.
const JOURNAL_MARKER: &str = "journal-instance-for-test";

#[derive(Default)]
struct FakeJournal {
    capability: Mutex<VecDeque<Capability>>,
    current_capability: Mutex<Option<Capability>>,
    vaults: Mutex<Option<VaultList>>,
    answers: Mutex<VecDeque<DeliveryResult>>,
    delivered: Mutex<Vec<(String, Delivery)>>,
    capability_calls: Mutex<u32>,
    pairing_changed: AtomicBool,
    name: Mutex<String>,
    /// Runs as an upload starts, which then never finishes.
    hang_after: Mutex<Option<Box<dyn FnOnce() + Send>>>,
}

impl FakeJournal {
    fn named(name: &str) -> Arc<Self> {
        let journal = Arc::new(Self::default());
        *journal.name.lock().unwrap() = name.to_owned();
        journal
    }
    fn delivered_count(&self) -> usize {
        self.delivered.lock().unwrap().len()
    }
}

impl Journal for Arc<FakeJournal> {
    fn capability(&self) -> BoxFuture<'_, Capability> {
        Box::pin(async move {
            *self.capability_calls.lock().unwrap() += 1;
            let next = self.capability.lock().unwrap().pop_front();
            if let Some(next) = next {
                *self.current_capability.lock().unwrap() = Some(next);
            }
            self.current_capability
                .lock()
                .unwrap()
                .unwrap_or(Capability::Supported)
        })
    }
    fn vault_list(&self) -> BoxFuture<'_, VaultList> {
        Box::pin(async move {
            self.vaults
                .lock()
                .unwrap()
                .take()
                .unwrap_or(VaultList::Listed(Vec::new()))
        })
    }
    fn deliver<'a>(&'a self, delivery: &'a Delivery) -> BoxFuture<'a, DeliveryResult> {
        Box::pin(async move {
            let name = self.name.lock().unwrap().clone();
            self.delivered
                .lock()
                .unwrap()
                .push((name, delivery.clone()));
            let hang = self.hang_after.lock().unwrap().take();
            if let Some(started) = hang {
                started();
                std::future::pending::<()>().await;
            }
            self.answers
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or(DeliveryResult::Answer(Answer::Delivered))
        })
    }
    fn pairing_unchanged(&self) -> BoxFuture<'_, bool> {
        Box::pin(async move { !self.pairing_changed.swap(false, Ordering::AcqRel) })
    }
}

struct Fixture {
    temp: tempfile::TempDir,
    vault: PathBuf,
    clock: Arc<AtomicI64>,
    journal: Arc<FakeJournal>,
    worker: Worker<Arc<FakeJournal>>,
    handle: ObsidianHandle,
    paused: Arc<AtomicBool>,
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
    .unwrap();
    let journal = FakeJournal::named(JOURNAL_MARKER);
    let paused = Arc::new(AtomicBool::new(false));
    let read_paused = Arc::clone(&paused);
    let (handle, worker) = Worker::new(
        Some(engine),
        Arc::clone(&journal),
        Arc::new(move || read_paused.load(Ordering::SeqCst)),
    );
    Fixture {
        temp,
        vault,
        clock,
        journal,
        worker,
        handle,
        paused,
    }
}

fn write_note(vault: &Path, name: &str, text: &str, mtime_ms: i64) {
    let path = vault.join(name);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, text).unwrap();
    fs::File::options()
        .write(true)
        .open(&path)
        .unwrap()
        .set_modified(std::time::UNIX_EPOCH + Duration::from_millis(mtime_ms as u64))
        .unwrap();
}

/// Drive the worker through a turn-on: the vault check, then the first scan.
async fn turn_on(f: &mut Fixture) -> ObsidianDocument {
    let (reply, answer) = oneshot::channel();
    f.worker
        .handle(Command::TurnOn {
            path: f.vault.to_str().unwrap().to_owned(),
            reply,
        })
        .await;
    let document = answer.await.unwrap();
    f.worker.step().await;
    f.worker.step().await;
    document
}

async fn steps_until_quiet(f: &mut Fixture) {
    for _ in 0..50 {
        f.worker.next_scan = Instant::now() + Duration::from_secs(3600);
        if !f.worker.step().await {
            return;
        }
    }
    panic!("the source never settled");
}

fn state(f: &Fixture) -> ObsidianDocument {
    f.worker.publish();
    f.handle.document()
}

fn files_under(dir: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    for entry in fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            found.extend(files_under(&path));
        } else {
            found.push(path);
        }
    }
    found
}

fn dump_tree(dir: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    let mut tree: Vec<_> = files_under(dir)
        .into_iter()
        .map(|path| {
            let bytes = fs::read(&path).unwrap();
            (path, bytes)
        })
        .collect();
    tree.sort();
    tree
}

fn state_dir_holds(temp: &Path, needle: &str) -> bool {
    files_under(&temp.join("state"))
        .iter()
        .any(|path| String::from_utf8_lossy(&fs::read(path).unwrap_or_default()).contains(needle))
}

#[tokio::test]
async fn a_journal_without_the_capability_holds_and_delivers_later() {
    let mut f = fixture();
    *f.journal.current_capability.lock().unwrap() = Some(Capability::NotListed);
    write_note(&f.vault, "a.md", "alpha", T0 - 3_600_000);
    turn_on(&mut f).await;
    f.worker.step().await;
    assert_eq!(f.journal.delivered_count(), 0, "nothing sent");
    assert_eq!(
        state(&f).status.state,
        State::WaitingForJournalUpdate,
        "waiting, not checking and not a fault"
    );

    // The journal is updated: only now does the vault check happen, then the send.
    *f.journal.current_capability.lock().unwrap() = Some(Capability::Supported);
    f.worker.capability_checked = None;
    f.worker.step().await;
    f.worker.next_scan = Instant::now();
    f.worker.step().await;
    assert_eq!(f.journal.delivered_count(), 1, "delivered now");
    assert_eq!(state(&f).status.state, State::On);
}

#[tokio::test]
async fn the_vault_check_waits_for_a_journal_that_takes_notes() {
    let mut f = fixture();
    *f.journal.current_capability.lock().unwrap() = Some(Capability::NotListed);
    *f.journal.vaults.lock().unwrap() = Some(VaultList::Listed(vec![ListedVault {
        vault: "Notes".into(),
        device: Some("studio".into()),
        last_synced_at: None,
    }]));
    turn_on(&mut f).await;
    assert!(
        f.journal.vaults.lock().unwrap().is_some(),
        "the list wasn't asked for yet"
    );

    *f.journal.current_capability.lock().unwrap() = Some(Capability::Supported);
    f.worker.capability_checked = None;
    f.worker.step().await;
    assert_eq!(state(&f).status.state, State::Confirm);
    assert_eq!(f.journal.delivered_count(), 0);
}

#[tokio::test]
async fn a_file_gone_from_the_outbox_is_sent_again_from_the_vault() {
    let mut f = fixture();
    write_note(&f.vault, "a.md", "alpha", T0 - 7_200_000);
    f.journal
        .answers
        .lock()
        .unwrap()
        .push_back(DeliveryResult::FileMissing);
    turn_on(&mut f).await;
    assert_eq!(f.journal.delivered_count(), 1);
    let doc = state(&f);
    assert_eq!(doc.status.attention, Some(Attention::FileRefused));
    assert_eq!(doc.status.waiting, 0, "the entry was dropped");

    // The next scan reports the note again, and it goes through.
    f.worker.next_scan = Instant::now();
    f.clock.fetch_add(11 * 60_000, Ordering::SeqCst);
    f.worker.step().await;
    f.worker.next_scan = Instant::now() + Duration::from_secs(3600);
    f.clock.fetch_add(11 * 60_000, Ordering::SeqCst);
    f.worker.step().await;
    assert_eq!(f.journal.delivered_count(), 2, "sent again");
    assert_eq!(state(&f).status.attention, None);
}

#[tokio::test]
async fn pause_holds_windows_and_delivery_and_resume_delivers() {
    let mut f = fixture();
    write_note(&f.vault, "a.md", "alpha", T0 - 3_600_000);
    f.paused.store(true, Ordering::SeqCst);
    turn_on(&mut f).await;
    f.worker.next_scan = Instant::now() + Duration::from_secs(3600);
    f.worker.step().await;
    assert_eq!(f.journal.delivered_count(), 0);
    let doc = state(&f);
    assert_eq!(doc.status.state, State::Paused);
    assert_eq!(doc.status.waiting, 1, "scanned but held");

    f.paused.store(false, Ordering::SeqCst);
    f.worker.next_scan = Instant::now() + Duration::from_secs(3600);
    f.worker.step().await;
    assert_eq!(f.journal.delivered_count(), 1);
    assert_eq!(state(&f).status.state, State::On);
}

#[tokio::test]
async fn a_vault_listed_by_the_journal_sends_nothing_until_the_owner_continues() {
    let mut f = fixture();
    write_note(&f.vault, "a.md", "alpha", T0 - 3_600_000);
    *f.journal.vaults.lock().unwrap() = Some(VaultList::Listed(vec![ListedVault {
        vault: "Notes".into(),
        device: Some("studio".into()),
        last_synced_at: Some("2026-11-08T09:44:33-07:00".into()),
    }]));
    turn_on(&mut f).await;
    steps_until_quiet(&mut f).await;
    assert_eq!(
        f.journal.delivered_count(),
        0,
        "nothing sent before the answer"
    );
    let doc = state(&f);
    assert_eq!(doc.status.state, State::Confirm);
    let warning = doc.status.warning.unwrap();
    assert_eq!(warning.vault, "Notes");
    assert_eq!(warning.device.as_deref(), Some("studio"));
    assert_eq!(
        warning.last_synced_at.as_deref(),
        Some("2026-11-08T09:44:33-07:00")
    );

    let (reply, _answer) = oneshot::channel();
    f.worker.handle(Command::Continue(reply)).await;
    f.worker.step().await;
    f.worker.next_scan = Instant::now() + Duration::from_secs(3600);
    f.worker.step().await;
    assert_eq!(f.journal.delivered_count(), 1);
    assert_eq!(state(&f).status.state, State::On);
}

#[tokio::test]
async fn cancelling_the_warning_leaves_the_source_off_and_sends_nothing() {
    let mut f = fixture();
    write_note(&f.vault, "a.md", "alpha", T0 - 3_600_000);
    *f.journal.vaults.lock().unwrap() = Some(VaultList::Listed(vec![ListedVault {
        vault: "Notes".into(),
        device: None,
        last_synced_at: None,
    }]));
    turn_on(&mut f).await;
    steps_until_quiet(&mut f).await;
    let (reply, answer) = oneshot::channel();
    f.worker.handle(Command::Cancel(reply)).await;
    assert_eq!(answer.await.unwrap().status.state, State::Off);
    assert_eq!(f.journal.delivered_count(), 0);
    assert_eq!(state(&f).status.waiting, 0);
}

#[tokio::test]
async fn an_older_journal_without_the_route_is_an_empty_list() {
    let mut f = fixture();
    write_note(&f.vault, "a.md", "alpha", T0 - 3_600_000);
    *f.journal.vaults.lock().unwrap() = Some(VaultList::Listed(Vec::new()));
    turn_on(&mut f).await;
    f.worker.next_scan = Instant::now() + Duration::from_secs(3600);
    f.worker.step().await;
    assert_eq!(state(&f).status.state, State::On);
    assert_eq!(f.journal.delivered_count(), 1);
}

#[tokio::test]
async fn a_new_pairing_re_asks_the_journal_before_sending() {
    let mut f = fixture();
    write_note(&f.vault, "a.md", "aaa", T0 - 7_200_000);
    turn_on(&mut f).await;
    steps_until_quiet(&mut f).await;
    assert_eq!(f.journal.delivered_count(), 1);
    let asks = *f.journal.capability_calls.lock().unwrap();

    write_note(&f.vault, "b.md", "bbb", T0 - 3_600_000);
    f.worker.next_scan = Instant::now();
    f.journal.pairing_changed.store(true, Ordering::Release);
    *f.journal.current_capability.lock().unwrap() = Some(Capability::NotListed);
    f.worker.step().await;
    assert_eq!(
        *f.journal.capability_calls.lock().unwrap(),
        asks + 1,
        "the new pairing is asked first"
    );
    assert_eq!(f.journal.delivered_count(), 1, "nothing went");
    assert_eq!(state(&f).status.state, State::WaitingForJournalUpdate);
}

#[tokio::test]
async fn a_refused_folder_is_reported_and_leaves_the_source_off() {
    let mut f = fixture();
    let plain = f.vault.parent().unwrap().join("plain");
    fs::create_dir_all(&plain).unwrap();
    let (reply, answer) = oneshot::channel();
    f.worker
        .handle(Command::TurnOn {
            path: plain.to_str().unwrap().to_owned(),
            reply,
        })
        .await;
    let doc = answer.await.unwrap();
    assert_eq!(doc.refusal, Some(Refusal::NotAVault));
    assert_eq!(doc.status.state, State::Off);
    assert_eq!(f.journal.delivered_count(), 0);
}

#[tokio::test]
async fn the_answers_table_is_honoured_end_to_end() {
    let mut f = fixture();
    // Oldest window first: a.md's file is refused invalid. b.md's gets no answer,
    // then a retry, then an ahead hold, and finally goes through; c.md's follows.
    write_note(&f.vault, "a.md", "aaa", T0 - 7_200_000);
    write_note(&f.vault, "b.md", "bbb", T0 - 6_000_000);
    write_note(&f.vault, "c.md", "ccc", T0 - 4_800_000);
    f.journal.answers.lock().unwrap().extend([
        DeliveryResult::Answer(Answer::Invalid),
        DeliveryResult::NoAnswer,
        DeliveryResult::Answer(Answer::Retry),
        DeliveryResult::Answer(Answer::Ahead),
    ]);
    turn_on(&mut f).await;
    f.worker.step().await;
    // Invalid is set aside and the rest carry on: the next file is offered.
    assert_eq!(state(&f).status.state, State::NeedsAttention);
    assert_eq!(
        state(&f).status.waiting,
        2,
        "the refused file no longer waits"
    );

    // No answer changes nothing: the file is offered again.
    f.worker.step().await;
    assert_eq!(f.journal.delivered_count(), 3);
    assert_eq!(state(&f).status.waiting, 2);

    // Retry backs off: nothing goes until the backoff has passed.
    f.worker.step().await;
    assert_eq!(f.journal.delivered_count(), 3, "backing off");
    f.clock.fetch_add(120_000, Ordering::SeqCst);
    f.worker.step().await;
    assert_eq!(f.journal.delivered_count(), 4, "tried again, ahead now");

    // Ahead holds the file for a few minutes and it is still waiting.
    f.worker.step().await;
    assert_eq!(f.journal.delivered_count(), 4, "held");
    assert_eq!(state(&f).status.waiting, 2);
    f.clock.fetch_add(6 * 60_000, Ordering::SeqCst);
    for _ in 0..3 {
        f.worker.next_scan = Instant::now() + Duration::from_secs(3600);
        f.worker.step().await;
    }
    let names: Vec<String> = f
        .journal
        .delivered
        .lock()
        .unwrap()
        .iter()
        .map(|(_, d)| d.name.clone())
        .collect();
    assert_eq!(f.journal.delivered_count(), 6, "every file went: {names:?}");
    let doc = state(&f);
    assert_eq!(doc.status.waiting, 0);
    // A refused file still needs attention; the app can't send it again.
    assert_eq!(doc.status.state, State::NeedsAttention);
}

#[tokio::test]
async fn held_files_carry_no_journal_identity_and_go_to_the_journal_paired_now() {
    let mut f = fixture();
    f.paused.store(true, Ordering::SeqCst);
    write_note(&f.vault, "a.md", "held while paused", T0 - 3_600_000);
    turn_on(&mut f).await;
    f.worker.next_scan = Instant::now() + Duration::from_secs(3600);
    f.worker.step().await;
    assert!(
        !state_dir_holds(f.temp.path(), JOURNAL_MARKER),
        "held state names a journal"
    );

    // The owner pairs another journal while the file waits.
    *f.journal.name.lock().unwrap() = "the journal paired now".to_owned();
    f.journal.pairing_changed.store(true, Ordering::Release);
    f.paused.store(false, Ordering::SeqCst);
    f.worker.next_scan = Instant::now() + Duration::from_secs(3600);
    f.worker.step().await;
    let delivered = f.journal.delivered.lock().unwrap().clone();
    assert_eq!(delivered.len(), 1);
    assert_eq!(delivered[0].0, "the journal paired now");
    assert!(!state_dir_holds(f.temp.path(), JOURNAL_MARKER));
}

#[tokio::test]
async fn turning_it_off_keeps_what_is_waiting_for_later() {
    let mut f = fixture();
    f.paused.store(true, Ordering::SeqCst);
    write_note(&f.vault, "a.md", "kept", T0 - 3_600_000);
    turn_on(&mut f).await;
    f.worker.next_scan = Instant::now() + Duration::from_secs(3600);
    f.worker.step().await;
    assert_eq!(state(&f).status.waiting, 1);

    let (reply, answer) = oneshot::channel();
    f.worker.handle(Command::TurnOff(reply)).await;
    let doc = answer.await.unwrap();
    assert_eq!(doc.status.state, State::Off);

    f.paused.store(false, Ordering::SeqCst);
    turn_on(&mut f).await;
    f.worker.next_scan = Instant::now() + Duration::from_secs(3600);
    f.worker.step().await;
    assert_eq!(f.journal.delivered_count(), 1, "the waiting file went");
}

#[tokio::test]
async fn turning_off_does_not_wait_on_an_upload() {
    let mut f = fixture();
    write_note(&f.vault, "a.md", "aaa", T0 - 7_200_000);
    let cancel = Arc::clone(&f.worker.cancel);
    let notify = Arc::clone(&f.worker.cancel_notify);
    *f.journal.hang_after.lock().unwrap() = Some(Box::new(move || {
        cancel.store(true, Ordering::Release);
        notify.notify_one();
    }));
    // Reaches the upload, which hangs until the cancel wins the race.
    turn_on(&mut f).await;
    assert_eq!(f.journal.delivered_count(), 1, "it started");
    assert_eq!(state(&f).status.waiting, 1, "and the file is still held");

    // Cancelled before an upload starts, nothing is attempted.
    f.worker.next_scan = Instant::now() + Duration::from_secs(3600);
    f.worker.step().await;
    assert_eq!(f.journal.delivered_count(), 1);
}

#[tokio::test]
async fn the_on_signal_follows_turning_it_on_and_off() {
    let mut f = fixture();
    let mut on = f.handle.on_changes();
    assert!(!*on.borrow_and_update());
    turn_on(&mut f).await;
    f.worker.publish();
    assert!(on.has_changed().unwrap());
    assert!(*on.borrow_and_update());

    let (reply, answer) = oneshot::channel();
    f.worker.handle(Command::TurnOff(reply)).await;
    answer.await.unwrap();
    assert!(on.has_changed().unwrap());
    assert!(!*on.borrow_and_update());
}

#[tokio::test]
async fn the_vault_is_byte_identical_after_a_full_run() {
    let mut f = fixture();
    write_note(&f.vault, "a.md", "alpha", T0 - 7_200_000);
    write_note(&f.vault, "sub/b.md", "beta", T0 - 3_600_000);
    fs::write(f.vault.join(".obsidian/app.json"), b"{}").unwrap();
    let before = dump_tree(&f.vault);
    turn_on(&mut f).await;
    steps_until_quiet(&mut f).await;
    assert_eq!(f.journal.delivered_count(), 2);
    assert_eq!(dump_tree(&f.vault), before);
}

#[tokio::test]
async fn a_restart_sends_the_vault_again_after_the_same_name_check() {
    let mut f = fixture();
    write_note(&f.vault, "a.md", "aaa", T0 - 7_200_000);
    turn_on(&mut f).await;
    steps_until_quiet(&mut f).await;
    assert_eq!(f.journal.delivered_count(), 1);
    let asks = *f.journal.capability_calls.lock().unwrap();

    *f.journal.vaults.lock().unwrap() = Some(VaultList::Listed(vec![ListedVault {
        vault: "Notes".into(),
        device: Some("studio".into()),
        last_synced_at: None,
    }]));
    f.handle.restart_send();
    f.worker.restart_if_asked().await;
    let doc = state(&f);
    assert_eq!(doc.status.state, State::Checking);
    assert_eq!(doc.status.vault.as_deref(), Some("Notes"), "the same vault");
    steps_until_quiet(&mut f).await;
    assert!(
        *f.journal.capability_calls.lock().unwrap() > asks,
        "the journal is asked again"
    );
    assert_eq!(state(&f).status.state, State::Confirm);
    assert_eq!(
        f.journal.delivered_count(),
        1,
        "not before the owner continues"
    );

    let (reply, _answer) = oneshot::channel();
    f.worker.handle(Command::Continue(reply)).await;
    f.worker.step().await;
    steps_until_quiet(&mut f).await;
    let delivered = f.journal.delivered.lock().unwrap().clone();
    assert_eq!(delivered.len(), 2, "the note went again");
    assert_eq!(delivered[1].1.first_sight, 1);
    let doc = state(&f);
    assert_eq!(doc.status.state, State::On);
    assert_eq!(doc.status.first_sync, None, "and the first send finished");
}

#[tokio::test]
async fn a_changed_pairing_alone_sends_nothing_again() {
    let mut f = fixture();
    write_note(&f.vault, "a.md", "aaa", T0 - 7_200_000);
    turn_on(&mut f).await;
    steps_until_quiet(&mut f).await;
    assert_eq!(f.journal.delivered_count(), 1);

    // A reconnect or a relaunch: asked again, but no restart.
    f.journal.pairing_changed.store(true, Ordering::Release);
    f.worker.restart_if_asked().await;
    f.worker.next_scan = Instant::now();
    f.worker.step().await;
    steps_until_quiet(&mut f).await;
    assert_eq!(f.journal.delivered_count(), 1);
    assert_eq!(state(&f).status.state, State::On);
}

#[tokio::test]
async fn the_running_source_takes_a_restart_without_a_command() {
    let mut f = fixture();
    write_note(&f.vault, "a.md", "aaa", T0 - 7_200_000);
    turn_on(&mut f).await;
    steps_until_quiet(&mut f).await;
    assert_eq!(f.journal.delivered_count(), 1);

    let Fixture {
        worker,
        handle,
        journal,
        temp: _temp,
        ..
    } = f;
    let task = tokio::spawn(worker.run());
    handle.restart_send();
    tokio::time::timeout(Duration::from_secs(20), async {
        while journal.delivered_count() < 2 {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the note went again");
    task.abort();
}
