// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! The screenshots source on Windows: the screenshots the owner takes, each
//! sent once to the paired journal at the second it was taken.
//!
//! Windows' own Screenshots folder gives up every image in it. A folder the
//! owner chooses gives up only what Windows' tools named (Snipping Tool's
//! dated name, the older `Screenshot (N)`) or Game Bar marked. Nothing is read
//! until the owner turns the source on, and nothing comes in until they choose
//! how far back to start. This crate holds the rules, the ledger and the
//! schedule; the app supplies the folder ([`Platform`]), the journal seam
//! ([`Journal`]) and the computer's pause.

#![forbid(unsafe_code)]

mod engine;
pub mod exif;
pub mod ledger;
pub mod rules;
mod worker;

pub use engine::{
    prepare, sha256_hex, Backlog, ConvertError, Engine, ListError, Listed, Pause, Platform,
    Prepared, SendJob, Sent, Status, Upload,
};
pub use observer_model::ScreenshotsState;
pub use rules::{Answer, Span, TakenFrom};
pub use worker::{
    BoxFuture, Capability, Delivery, Journal, Paused, ScreenshotsDocument, ScreenshotsHandle,
    CAPABILITY, CAPABILITY_RETRY, SENDS_PER_LOOK, SOURCE,
};

use std::future::Future;
use std::path::PathBuf;
use std::sync::Arc;

use worker::Worker;

/// Build the source: a handle for the rest of the app and the background work,
/// which the caller runs on its async runtime. The ledger is read with the
/// source's own work, never in the way of the app starting. A failure to open
/// leaves the rest of the app running, with this source unavailable.
pub fn start<J: Journal>(
    dir: PathBuf,
    platform: Arc<dyn Platform>,
    journal: J,
    paused: Paused,
    counters: Option<source_counters::SharedCounters>,
) -> (ScreenshotsHandle, impl Future<Output = ()> + Send + 'static) {
    let (handle, mut worker) = Worker::new(journal, paused);
    (handle, async move {
        let opened =
            tokio::task::spawn_blocking(move || Engine::open(dir, platform, counters)).await;
        match opened {
            Ok(Ok(engine)) => {
                worker.set_engine(engine);
                worker.publish();
                worker.run().await;
            }
            Ok(Err(error)) => {
                tracing::warn!(target: "screenshots", %error, "screenshots source is unavailable")
            }
            Err(error) => {
                tracing::warn!(target: "screenshots", %error, "screenshots source is unavailable")
            }
        }
    })
}
