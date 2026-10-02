// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Delivery of finalized periods to the paired journal as the `browser` source.
//!
//! One pass takes one view of the journal (the identity and the client that
//! would carry the upload come from the same credential) and sends each
//! finalized period of the active generation, oldest first. A period is
//! released only on a receipt that proves the journal holds its exact bytes;
//! the journal answers a re-sent period as a duplicate of the stored one, so a
//! lost response costs a resend, never a second copy.
//!
//! Custody bound to a different journal is never sent: if the view's identity
//! is not the active generation's, the pass stops and the gate update retires
//! it.

use std::future::Future;

use crate::custody::OutboxEntry;
use crate::hub::Hub;

/// The result of one upload attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UploadOutcome {
    /// The journal holds these exact bytes.
    Delivered,
    /// Sending is held (journal mark unanswered, or no link right now); not a
    /// failure, try again later.
    Held,
    /// A contract failure code: `relay_unavailable`, `journal_rejected`, or
    /// `local_io`.
    Failed(&'static str),
}

/// One view of the paired journal.
pub trait Journal {
    /// The strict identity of the journal this view uploads to.
    fn identity(&self) -> Option<&str>;
    /// Upload `body` (the period's `browser_pages.jsonl`) for `entry`.
    fn upload(
        &self,
        entry: &OutboxEntry,
        body: Vec<u8>,
    ) -> impl Future<Output = UploadOutcome> + Send;
}

/// Summary of one pass, for logs and the delivery state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PassSummary {
    pub delivered: usize,
    pub remaining: usize,
}

/// Run one delivery pass.
pub async fn deliver_once<J: Journal>(hub: &Hub, journal: &J) -> PassSummary {
    let mut summary = PassSummary::default();
    let Some(identity) = journal.identity() else {
        return summary;
    };
    if hub.identity().as_deref() != Some(identity) {
        return summary;
    }
    let entries = hub.outbox();
    summary.remaining = entries.len();
    for entry in entries {
        let body = match std::fs::read(entry.pages_path()) {
            Ok(body) => body,
            Err(_) => {
                hub.set_delivery_failure(Some("local_io"));
                return summary;
            }
        };
        if body.len() as u64 != entry.size {
            hub.set_delivery_failure(Some("local_io"));
            return summary;
        }
        match journal.upload(&entry, body).await {
            UploadOutcome::Delivered => {
                if hub.delivered(&entry).is_err() {
                    hub.set_delivery_failure(Some("local_io"));
                    return summary;
                }
                summary.delivered += 1;
                summary.remaining -= 1;
            }
            UploadOutcome::Held => return summary,
            UploadOutcome::Failed(code) => {
                hub.set_delivery_failure(Some(code));
                return summary;
            }
        }
    }
    hub.set_delivery_failure(None);
    summary
}
