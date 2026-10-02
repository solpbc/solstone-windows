// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Upload one file into a device sub-stream (`source`) and prove the journal
//! holds it.
//!
//! The capture coordinator owns the primary screen/audio stream. A sub-stream
//! with its own custody (the browser source) uses this instead: one request,
//! released by the caller only on a receipt naming the exact bytes. The
//! journal answers a repeated upload of the same bytes as a duplicate of the
//! stored segment, so a lost response costs a resend, never a second copy.

use observer_model::LocalZone;
use observer_pl::ingest::{validate_receipt, FilePart, LocalFile, ReceiptFault};
use spl_core::ca;

use crate::{ObserverClient, RouteError, TransportError};

/// The outcome of one attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceUploadOutcome {
    /// The receipt proves the journal holds these exact bytes.
    Stored,
    /// Sending is held until the owner confirms the journal mark.
    Held,
    /// The journal could not be reached, or did not finish writing; retry.
    Unreachable,
    /// The journal refused the upload or answered with a receipt that does not
    /// prove custody.
    Rejected,
}

pub struct SourceFile<'a> {
    pub source: &'a str,
    pub day: &'a str,
    pub segment: &'a str,
    pub zone: &'a LocalZone,
    pub filename: &'a str,
    pub content_type: &'a str,
    pub bytes: Vec<u8>,
}

pub async fn upload_source_file(
    client: &ObserverClient,
    file: SourceFile<'_>,
) -> SourceUploadOutcome {
    let sha256 = ca::sha256_hex(&file.bytes);
    let size = file.bytes.len() as u64;
    let parts = vec![FilePart {
        filename: file.filename.to_string(),
        content_type: file.content_type.to_string(),
        bytes: file.bytes,
    }];
    let response = client
        .ingest_source(
            file.segment,
            file.day,
            parts,
            Some(file.zone),
            Some(file.source),
        )
        .await;
    match response {
        Ok((response, _)) => {
            let local = [LocalFile {
                name: file.filename,
                sha256: &sha256,
                size,
            }];
            match validate_receipt(&response, &local) {
                Ok(_) => SourceUploadOutcome::Stored,
                Err(ReceiptFault::ReceivedNotWritten { .. }) => SourceUploadOutcome::Unreachable,
                Err(_) => SourceUploadOutcome::Rejected,
            }
        }
        Err(RouteError::AwaitingConfirmation) => SourceUploadOutcome::Held,
        Err(RouteError::Transport(TransportError::Rejected { status, .. })) if status < 500 => {
            SourceUploadOutcome::Rejected
        }
        Err(_) => SourceUploadOutcome::Unreachable,
    }
}
