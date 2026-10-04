// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Delivery of finalized browser periods to whichever confirmed journal is
//! current for each individual send.

use std::future::Future;

use crate::custody::OutboxEntry;
use crate::hub::Hub;

/// The result of one upload attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UploadOutcome {
    Delivered,
    Held,
    Failed(&'static str),
}

/// One view of a journal connection.
pub trait Journal {
    fn same_connection(&self, current: &Self) -> bool;

    fn upload(
        &self,
        entry: &OutboxEntry,
        body: Vec<u8>,
    ) -> impl Future<Output = UploadOutcome> + Send;
}

/// Summary of one pass, for logs and delivery state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PassSummary {
    pub delivered: usize,
    pub remaining: usize,
}

struct Reservation<'a> {
    hub: &'a Hub,
    entry: OutboxEntry,
    armed: bool,
}

impl Drop for Reservation<'_> {
    fn drop(&mut self) {
        if self.armed {
            self.hub.release(&self.entry);
        }
    }
}

/// Deliver pending periods oldest first, reloading the client before every
/// send and after every result. A reserved period is never discarded while its
/// upload is in flight.
pub async fn deliver_pending<J, F, Fut>(hub: &Hub, mut load: F) -> PassSummary
where
    J: Journal,
    F: FnMut() -> Fut,
    Fut: Future<Output = Option<J>> + Send,
{
    let entries = hub.outbox();
    let mut summary = PassSummary {
        delivered: 0,
        remaining: entries.len(),
    };
    for entry in entries {
        let Some(journal) = load().await else {
            return summary;
        };
        if !hub.reserve(&entry) {
            continue;
        }
        let mut reservation = Reservation {
            hub,
            entry: entry.clone(),
            armed: true,
        };
        let body = match std::fs::read(entry.pages_path()) {
            Ok(body) if body.len() as u64 == entry.size => body,
            _ => {
                let current = load().await;
                let still = current
                    .as_ref()
                    .is_some_and(|current| journal.same_connection(current));
                if still {
                    hub.set_delivery_failure(Some("local_io"));
                }
                return summary;
            }
        };
        let outcome = journal.upload(&entry, body).await;
        let current = load().await;
        let still = current
            .as_ref()
            .is_some_and(|current| journal.same_connection(current));
        match outcome {
            UploadOutcome::Delivered => {
                if hub.delivered(&entry).is_err() {
                    if still {
                        hub.set_delivery_failure(Some("local_io"));
                    }
                    return summary;
                }
                reservation.armed = false;
                summary.delivered += 1;
                summary.remaining = summary.remaining.saturating_sub(1);
                if still {
                    hub.set_delivery_failure(None);
                }
            }
            UploadOutcome::Held => {
                if still {
                    hub.set_delivery_failure(None);
                }
                return summary;
            }
            UploadOutcome::Failed(code) => {
                if still {
                    hub.set_delivery_failure(Some(code));
                }
                return summary;
            }
        }
    }
    summary
}
