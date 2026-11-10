// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! The Obsidian vault source on Windows: the shared engine run as a background
//! task, delivering over the app's paired upload path as the `obsidian` source.
//!
//! This crate holds the schedule and the rules; the app supplies the journal
//! seam ([`Journal`]) and whether capture is paused. It holds nothing that
//! names a journal.

mod worker;

pub use solstone_vault::platform::DirFileIds as ReadDirFileIds;
pub use solstone_vault::{
    Answer, Attention, Delivery, FoundVault, ListedVault, Phase, Refusal, State, Status, Window,
};
pub use worker::{
    decode_document, BoxFuture, Capability, DeliveryResult, Journal, ObsidianDocument,
    ObsidianHandle, Paused, VaultList, CAPABILITY, SOURCE,
};

use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use solstone_vault::platform::DirFileIds;
use solstone_vault::{Engine, SystemClock};
use worker::Worker;

/// Where Obsidian keeps its list of vaults, given `%APPDATA%`.
pub fn windows_candidate(app_data: &Path) -> PathBuf {
    app_data.join("obsidian").join("obsidian.json")
}

/// The vaults Obsidian itself lists on this computer. `app_data` is `%APPDATA%`.
pub fn found_vaults(app_data: &Path) -> Vec<FoundVault> {
    solstone_vault::found_vaults(&[windows_candidate(app_data)])
}

/// Build the source: a handle for the rest of the app and the background work,
/// which the caller runs on its async runtime. The first read of what's held can
/// be long, so it happens with the source's own work, never in the way of the app
/// starting. A failure to open leaves the rest of the app running, with this
/// source unavailable.
pub fn start<J: Journal>(
    root: PathBuf,
    dir_file_ids: Option<Arc<DirFileIds>>,
    journal: J,
    paused: Paused,
    counters: Option<source_counters::SharedCounters>,
) -> (ObsidianHandle, impl Future<Output = ()> + Send + 'static) {
    let (handle, worker) = Worker::new(None, journal, paused);
    (handle, async move {
        open_and_run(root, dir_file_ids, counters, worker).await
    })
}

fn note_intake(intake: &solstone_vault::Intake) -> source_counters::NoteIntake {
    source_counters::NoteIntake {
        id: intake.id,
        path: intake.path.clone(),
        previous_path: intake.previous_path.clone(),
        replaced: intake.replaced.clone(),
        first_sight: intake.first_sight,
        removed: intake.removed,
        inventory: intake.inventory,
        at_secs: Some(intake.at_ms.div_euclid(1000)),
    }
}

async fn open_and_run<J: Journal>(
    root: PathBuf,
    dir_file_ids: Option<Arc<DirFileIds>>,
    counters: Option<source_counters::SharedCounters>,
    mut worker: Worker<J>,
) {
    let opened = tokio::task::spawn_blocking(move || {
        let mut engine = Engine::open(root, Arc::new(SystemClock))?;
        if let Some(read) = dir_file_ids {
            engine = engine.with_dir_file_ids(read);
        }
        if let Some(counters) = counters {
            let paths = engine.known_note_paths();
            let intakes = engine.intakes();
            let mapped: Vec<_> = intakes.iter().map(note_intake).collect();
            if counters.reconcile_notes(&paths, &mapped) {
                let through = counters.last_intake_id();
                let _ = engine.ack_intakes(through);
            }
            let hooked = counters.clone();
            engine.set_intake_hook(std::sync::Arc::new(move |intakes| {
                let mapped: Vec<_> = intakes.iter().map(note_intake).collect();
                hooked.apply_intakes(&mapped)
            }));
        }
        Ok::<Engine, std::io::Error>(engine)
    })
    .await;
    match opened {
        Ok(Ok(engine)) => {
            worker.set_engine(engine);
            worker.publish();
            worker.run().await;
        }
        Ok(Err(error)) => {
            tracing::warn!(target: "obsidian", %error, "Obsidian vault source is unavailable")
        }
        Err(error) => {
            tracing::warn!(target: "obsidian", %error, "Obsidian vault source is unavailable")
        }
    }
}
