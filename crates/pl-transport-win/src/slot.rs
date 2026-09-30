// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Single-slot uploader supervisor.

use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{watch, Notify};
use tokio::task::JoinHandle;

const TEARDOWN_BUDGET: Duration = Duration::from_secs(5);

/// Exit signals sent to the uploader task.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlotExit {
    Run,
    Quiesce,
    Shutdown,
}

pub struct UploaderSlot {
    active: Option<Active>,
    wake: Arc<Notify>,
}

struct Active {
    cancel: watch::Sender<SlotExit>,
    task: JoinHandle<()>,
}

impl UploaderSlot {
    pub fn new() -> Self {
        Self {
            active: None,
            wake: Arc::new(Notify::new()),
        }
    }

    /// Wake notification handle for the upload loop.
    pub fn wake(&self) -> Arc<Notify> {
        self.wake.clone()
    }

    /// Kick the uploader loop immediately.
    pub fn kick(&self) {
        self.wake.notify_one();
    }

    /// Signal the incumbent with `Shutdown`, wait briefly for cooperative teardown,
    /// then spawn the replacement.
    pub async fn replace<F, Fut>(&mut self, build: F)
    where
        F: FnOnce(watch::Receiver<SlotExit>) -> Fut + Send,
        Fut: Future<Output = ()> + Send + 'static,
    {
        if let Some(prev) = self.active.take() {
            let _ = prev.cancel.send(SlotExit::Shutdown);
            if tokio::time::timeout(TEARDOWN_BUDGET, prev.task)
                .await
                .is_err()
            {
                tracing::warn!(
                    target: "sync",
                    reason = "uploader_teardown_timeout",
                    "uploader teardown timed out"
                );
            }
        }

        let (cancel, rx) = watch::channel(SlotExit::Run);
        let task = tokio::spawn(build(rx));
        self.active = Some(Active { cancel, task });
    }

    /// Quiesce and stop the active uploader task without calling post_connect retirement.
    pub async fn stop(&mut self) {
        if let Some(prev) = self.active.take() {
            let _ = prev.cancel.send(SlotExit::Quiesce);
            if tokio::time::timeout(TEARDOWN_BUDGET, prev.task)
                .await
                .is_err()
            {
                tracing::warn!(
                    target: "sync",
                    reason = "uploader_stop_timeout",
                    "uploader stop timed out"
                );
            }
        }
    }
}

impl Default for UploaderSlot {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    use tokio::sync::mpsc;

    fn log_snapshot(log: &Arc<Mutex<Vec<u8>>>) -> Vec<u8> {
        log.lock().unwrap().clone()
    }

    async fn wait_exit(rx: &mut watch::Receiver<SlotExit>) -> SlotExit {
        loop {
            let current = *rx.borrow();
            if current != SlotExit::Run {
                return current;
            }
            if rx.changed().await.is_err() {
                return SlotExit::Shutdown;
            }
        }
    }

    #[tokio::test]
    async fn replace_stops_incumbent_before_new_uploader_ticks() {
        let mut slot = UploaderSlot::new();
        let log = Arc::new(Mutex::new(Vec::new()));
        let (tick_tx, mut tick_rx) = mpsc::unbounded_channel();

        let a_log = log.clone();
        let a_tick_tx = tick_tx.clone();
        slot.replace(move |mut cancel| async move {
            a_log.lock().unwrap().push(1);
            let _ = a_tick_tx.send(1);
            let _ = wait_exit(&mut cancel).await;
            a_log.lock().unwrap().push(10);
        })
        .await;
        assert_eq!(tick_rx.recv().await, Some(1));

        let b_log = log.clone();
        let b_tick_tx = tick_tx;
        slot.replace(move |mut cancel| async move {
            b_log.lock().unwrap().push(2);
            let _ = b_tick_tx.send(2);
            let _ = wait_exit(&mut cancel).await;
        })
        .await;
        assert_eq!(tick_rx.recv().await, Some(2));

        assert_eq!(log_snapshot(&log), vec![1, 10, 2]);

        let active = slot.active.take().expect("active replacement");
        let _ = active.cancel.send(SlotExit::Shutdown);
        active.task.await.unwrap();
    }

    #[tokio::test]
    async fn skipped_replace_leaves_incumbent_running() {
        let mut slot = UploaderSlot::new();
        let (tick_tx, mut tick_rx) = mpsc::unbounded_channel();

        slot.replace(move |mut cancel| async move {
            loop {
                let _ = tick_tx.send(7);
                tokio::select! {
                    _ = wait_exit(&mut cancel) => break,
                    _ = tokio::time::sleep(Duration::from_millis(1)) => {}
                }
            }
        })
        .await;

        assert_eq!(tick_rx.recv().await, Some(7));
        assert_eq!(tick_rx.recv().await, Some(7));
        assert!(slot.active.is_some());

        let active = slot.active.take().expect("incumbent still active");
        let _ = active.cancel.send(SlotExit::Shutdown);
        active.task.await.unwrap();
    }
}
