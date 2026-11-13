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
use observer_pl::ingest::{
    validate_receipt, FilePart, IngestResponse, IngestStatus, LocalFile, ReceiptFault,
};
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

/// What a journal said about a file sent into a source whose journal-side
/// descriptors don't name the sent file (Obsidian's note files). Only the
/// status word and reason code are read; there is no custody to prove.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SourceFileReply {
    /// The journal answered. `status` is the ingest status word, when the body
    /// carried one; `reason_code` names what it refused or why it held.
    Answered {
        status: Option<String>,
        reason_code: Option<String>,
    },
    /// Sending is held until the owner confirms the journal mark.
    Held,
    /// The journal could not be reached.
    Unreachable,
}

/// What a journal said to one screenshot, for the source to route on the
/// contract's answers table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScreenshotReply {
    Answered {
        http_status: u16,
        status: Option<String>,
        reason_code: Option<String>,
    },
    /// Sending is held until the owner confirms the journal mark.
    Held,
    /// The journal could not be reached.
    Unreachable,
}

/// One screenshot, stamped at its capture second.
pub struct ScreenshotFile<'a> {
    pub day: &'a str,
    pub segment: &'a str,
    pub filename: &'a str,
    pub content_type: &'a str,
    pub captured_at: &'a str,
    pub taken_from: &'a str,
    pub bytes: Vec<u8>,
}

/// Send one screenshot into the `screenshots` sub-stream.
pub async fn upload_screenshot(
    client: &ObserverClient,
    file: ScreenshotFile<'_>,
) -> ScreenshotReply {
    let part = FilePart {
        filename: file.filename.to_string(),
        content_type: file.content_type.to_string(),
        bytes: file.bytes,
    };
    let response = client
        .ingest_screenshot(
            file.segment,
            file.day,
            part,
            file.captured_at,
            file.taken_from,
        )
        .await
        .map(|(response, _)| response);
    screenshot_reply(response)
}

fn screenshot_reply(response: Result<IngestResponse, RouteError>) -> ScreenshotReply {
    match response {
        Ok(response) => ScreenshotReply::Answered {
            http_status: match response.status {
                IngestStatus::Ok | IngestStatus::Duplicate | IngestStatus::Collision => 200,
                IngestStatus::Conflict => 409,
                IngestStatus::Failed => 500,
            },
            status: Some(status_word(response.status).to_string()),
            reason_code: response.reason_code,
        },
        Err(RouteError::AwaitingConfirmation) => ScreenshotReply::Held,
        Err(RouteError::Transport(TransportError::Rejected { status, body })) => {
            ScreenshotReply::Answered {
                http_status: status,
                status: None,
                reason_code: reason_code_of(&body),
            }
        }
        Err(_) => ScreenshotReply::Unreachable,
    }
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

/// Send one file into a device sub-stream and report the journal's answer
/// without judging it, for sources whose descriptors don't name the sent file.
pub async fn upload_source_file_for_reply(
    client: &ObserverClient,
    file: SourceFile<'_>,
) -> SourceFileReply {
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
        Ok((response, _)) => SourceFileReply::Answered {
            status: Some(status_word(response.status).to_string()),
            reason_code: response.reason_code,
        },
        Err(RouteError::AwaitingConfirmation) => SourceFileReply::Held,
        Err(RouteError::Transport(TransportError::Rejected { body, .. })) => {
            SourceFileReply::Answered {
                status: None,
                reason_code: reason_code_of(&body),
            }
        }
        Err(_) => SourceFileReply::Unreachable,
    }
}

fn status_word(status: IngestStatus) -> &'static str {
    match status {
        IngestStatus::Ok => "ok",
        IngestStatus::Duplicate => "duplicate",
        IngestStatus::Collision => "collision",
        IngestStatus::Conflict => "conflict",
        IngestStatus::Failed => "failed",
    }
}

/// The reason code in a refusal's JSON body, when it has one.
fn reason_code_of(body: &str) -> Option<String> {
    serde_json::from_str::<serde_json::Value>(body)
        .ok()?
        .get("reason_code")?
        .as_str()
        .map(str::to_owned)
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

#[cfg(test)]
mod tests {
    use super::{reason_code_of, screenshot_reply, ScreenshotReply};
    use crate::{RouteError, TransportError};
    use observer_pl::ingest::IngestResponse;

    fn answered(http_status: u16, status: Option<&str>, reason: Option<&str>) -> ScreenshotReply {
        ScreenshotReply::Answered {
            http_status,
            status: status.map(str::to_owned),
            reason_code: reason.map(str::to_owned),
        }
    }

    #[test]
    fn a_screenshot_answer_keeps_its_http_status_word_and_reason() {
        let ok: IngestResponse =
            serde_json::from_str(r#"{"status":"duplicate","existing_segment":"143207_1"}"#)
                .unwrap();
        assert_eq!(
            screenshot_reply(Ok(ok)),
            answered(200, Some("duplicate"), None)
        );
        let refused = TransportError::Rejected {
            status: 400,
            body: r#"{"error":"x","reason_code":"screenshot_invalid","detail":"y"}"#.into(),
        };
        assert_eq!(
            screenshot_reply(Err(RouteError::Transport(refused))),
            answered(400, None, Some("screenshot_invalid"))
        );
        let too_big = TransportError::Rejected {
            status: 413,
            body: "too large".into(),
        };
        assert_eq!(
            screenshot_reply(Err(RouteError::Transport(too_big))),
            answered(413, None, None)
        );
        assert_eq!(
            screenshot_reply(Err(RouteError::AwaitingConfirmation)),
            ScreenshotReply::Held
        );
    }

    #[test]
    fn a_refusal_reason_code_comes_out_of_a_json_body() {
        assert_eq!(
            reason_code_of(r#"{"error":"x","reason_code":"obsidian_note_invalid","detail":"y"}"#)
                .as_deref(),
            Some("obsidian_note_invalid")
        );
        assert_eq!(reason_code_of("not json"), None);
        assert_eq!(reason_code_of(r#"{"error":"x"}"#), None);
    }
}
