// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Test-only operation completion. Each pass owns separate retained channels;
//! request observation is deliberately not a response-processing receipt.

use tokio::sync::watch;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Processed,
    Failed,
    TimedOut,
    Cancelled,
}

#[derive(Clone)]
pub struct Attempt {
    pub token: (u64, u64, u64),
    metadata: watch::Receiver<Option<Outcome>>,
    access: watch::Receiver<Option<Outcome>>,
}

impl Attempt {
    pub(crate) fn new(token: (u64, u64, u64)) -> (Self, Completion, Completion) {
        let (metadata, metadata_rx) = watch::channel(None);
        let (access, access_rx) = watch::channel(None);
        (
            Self {
                token,
                metadata: metadata_rx,
                access: access_rx,
            },
            Completion(metadata),
            Completion(access),
        )
    }

    /// Also works after completion; a successor pass cannot satisfy this pass.
    pub async fn wait(mut self) -> (Outcome, Outcome) {
        (wait(&mut self.metadata).await, wait(&mut self.access).await)
    }
}

async fn wait(receiver: &mut watch::Receiver<Option<Outcome>>) -> Outcome {
    loop {
        if let Some(outcome) = *receiver.borrow_and_update() {
            return outcome;
        }
        if receiver.changed().await.is_err() {
            return Outcome::Cancelled;
        }
    }
}

pub(crate) struct Completion(watch::Sender<Option<Outcome>>);

impl Completion {
    pub(crate) fn finish(self, outcome: Outcome) {
        self.0.send_replace(Some(outcome));
    }
}

impl Drop for Completion {
    fn drop(&mut self) {
        self.0.send_if_modified(|value| {
            if value.is_some() {
                false
            } else {
                *value = Some(Outcome::Cancelled);
                true
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn late_waiter_sees_processed_and_failed_decisions() {
        let (attempt, metadata, access) = Attempt::new((1, 2, 3));
        metadata.finish(Outcome::Processed);
        access.finish(Outcome::Failed);
        assert_eq!(attempt.wait().await, (Outcome::Processed, Outcome::Failed));
    }

    #[tokio::test]
    async fn cancellation_and_timeout_are_not_processed() {
        let (attempt, metadata, access) = Attempt::new((1, 2, 3));
        drop(metadata);
        access.finish(Outcome::TimedOut);
        assert_eq!(
            attempt.wait().await,
            (Outcome::Cancelled, Outcome::TimedOut)
        );
    }

    #[tokio::test]
    async fn earlier_epoch_cannot_release_successor() {
        let (old, metadata, access) = Attempt::new((1, 2, 3));
        let (new, next_metadata, next_access) = Attempt::new((1, 4, 3));
        metadata.finish(Outcome::Processed);
        access.finish(Outcome::Processed);
        assert_eq!(old.wait().await, (Outcome::Processed, Outcome::Processed));
        assert!(new.metadata.borrow().is_none());
        assert!(new.access.borrow().is_none());
        next_metadata.finish(Outcome::Failed);
        next_access.finish(Outcome::Processed);
        assert_eq!(new.wait().await, (Outcome::Failed, Outcome::Processed));
    }
}
