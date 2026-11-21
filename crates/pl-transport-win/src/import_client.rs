// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! The linked-device import contract: streamed save, then idempotent start.

use observer_model::i18n::message;

use std::path::Path;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use spl_core::http::HttpResponse;
use tokio::io::AsyncReadExt;

use crate::ordinary_request::OrdinaryRequest;
use crate::{ObserverClient, RouteError};

pub use crate::streamed_request::REQUEST_BODY_LIMIT;
pub const MAX_IMPORT_FILE_BYTES: u64 = 4 * 1024 * 1024 * 1024;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct SavedImport {
    pub path: String,
    pub timestamp: String,
}

#[derive(Debug, PartialEq, Eq)]
pub enum ImportReply {
    Saved(SavedImport),
    Delivered,
    Retry,
    AuthStop,
    Refused {
        sentence: String,
        retryable: bool,
        clear_saved: bool,
    },
}

/// What the journal page asked for when it handed a file to the app: the import
/// area it was chosen for and the page's context, both passed through on save.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Handoff {
    pub area: String,
    pub context: serde_json::Map<String, serde_json::Value>,
}

/// Held file and its original display metadata. Only bounded framing is held
/// in memory; the payload stays on disk throughout delivery.
pub struct ImportFile<'a> {
    pub id: &'a str,
    pub name: &'a str,
    pub content_type: &'a str,
    pub path: &'a Path,
    pub handoff: Option<&'a Handoff>,
}

pub(crate) struct MultipartFile {
    pub(crate) prefix: Vec<u8>,
    pub(crate) suffix: Vec<u8>,
    pub(crate) content_type: String,
    pub(crate) length: u64,
}

impl MultipartFile {
    pub(crate) fn new(file: &ImportFile<'_>, size: u64) -> Self {
        let boundary = format!("----solstoneimport{}", file.id);
        let name = filename(file.name);
        let prefix = format!("--{boundary}\r\nContent-Disposition: form-data; name=\"client_item_id\"\r\n\r\n{}\r\n--{boundary}\r\nContent-Disposition: form-data; name=\"imported_via\"\r\n\r\nwindows\r\n--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"{name}\"\r\nContent-Type: {}\r\n\r\n", file.id, file.content_type).into_bytes();
        // The page's choices follow the file, as the Linux app sends them. The area
        // is a checked token and serialized JSON has no raw line breaks, so neither
        // can end a part early.
        let mut suffix = String::from("\r\n");
        if let Some(handoff) = file.handoff {
            let mut field = |name: &str, value: &str| {
                suffix.push_str(&format!("--{boundary}\r\nContent-Disposition: form-data; name=\"{name}\"\r\n\r\n{value}\r\n"));
            };
            field("area", &handoff.area);
            if !matches!(handoff.area.as_str(), "quick" | "recording") {
                field("source_hint", &handoff.area);
            }
            field(
                "context",
                &serde_json::to_string(&handoff.context).unwrap_or_else(|_| "{}".into()),
            );
        }
        suffix.push_str(&format!("--{boundary}--\r\n"));
        let suffix = suffix.into_bytes();
        Self {
            length: size
                .saturating_add(prefix.len() as u64)
                .saturating_add(suffix.len() as u64),
            prefix,
            suffix,
            content_type: format!("multipart/form-data; boundary={boundary}"),
        }
    }
}

fn filename(name: &str) -> String {
    let name = name.rsplit(['/', '\\']).next().unwrap_or("file");
    let name: String = name
        .chars()
        .filter(|c| !matches!(c, '"' | '\r' | '\n' | '\0'))
        .collect();
    let (stem, extension) = name
        .rsplit_once('.')
        .map_or((name.as_str(), "".to_string()), |(stem, ext)| {
            (stem, format!(".{ext}"))
        });
    let extension = if extension.len() < 128 {
        extension
    } else {
        String::new()
    };
    let mut end = stem.len().min(128 - extension.len());
    while !stem.is_char_boundary(end) {
        end -= 1;
    }
    if end == 0 && extension.is_empty() {
        "file".into()
    } else {
        format!("{}{extension}", &stem[..end])
    }
}

impl ImportFile<'_> {
    pub fn request_length(&self, size: u64) -> u64 {
        MultipartFile::new(self, size).length
    }
}

impl ObserverClient {
    pub async fn save_import_file(
        self: &Arc<Self>,
        file: ImportFile<'_>,
    ) -> Result<ImportReply, RouteError> {
        let payload = tokio::fs::File::open(file.path)
            .await
            .map_err(crate::TransportError::from)?;
        let size = payload
            .metadata()
            .await
            .map_err(crate::TransportError::from)?
            .len();
        let multipart = MultipartFile::new(&file, size);
        let reader = multipart
            .prefix
            .as_slice()
            .chain(payload.take(size))
            .chain(multipart.suffix.as_slice());
        let response = crate::streamed_request::post(
            self.clone(),
            OrdinaryRequest::ImportSavePost,
            &multipart.content_type,
            reader,
            multipart.length,
        )
        .await?;
        Ok(classify(&response, true, None))
    }

    pub async fn start_import(&self, saved: &SavedImport) -> Result<ImportReply, RouteError> {
        let body = serde_json::to_vec(saved).map_err(crate::TransportError::from)?;
        let headers = self.proxy_headers(&[("Content-Type".into(), "application/json".into())]);
        let (response, _) = self
            .ordinary_request(OrdinaryRequest::ImportStartPost, None, &headers, &body)
            .await?;
        Ok(classify(&response, false, Some(&saved.timestamp)))
    }
}

fn nonempty(value: &serde_json::Value, key: &str) -> Option<String> {
    value
        .get(key)?
        .as_str()
        .filter(|v| !v.is_empty())
        .map(str::to_owned)
}

pub(crate) fn classify(
    response: &HttpResponse,
    save: bool,
    expected_import: Option<&str>,
) -> ImportReply {
    if matches!(response.status, 401 | 403) {
        return ImportReply::AuthStop;
    }
    if response.status >= 500 {
        return ImportReply::Retry;
    }
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(&response.body) else {
        return if (200..300).contains(&response.status) {
            ImportReply::Retry
        } else {
            ImportReply::Refused {
                sentence: message("windows.ui.yourjournalcouldntacceptthis", &[]),
                retryable: false,
                clear_saved: false,
            }
        };
    };
    if response.status == 400
        && value["reason_code"] == "invalid_operation_for_state"
        && value["kind"] == "already_imported"
    {
        return ImportReply::Delivered;
    }
    if response.status == 200 {
        if save && value["status"] == "staged" {
            if let (Some(path), Some(timestamp)) =
                (nonempty(&value, "path"), nonempty(&value, "timestamp"))
            {
                return ImportReply::Saved(SavedImport { path, timestamp });
            }
        } else if !save
            && value["status"] == "ok"
            && nonempty(&value, "task_id").is_some()
            && nonempty(&value, "import_id").as_deref() == expected_import
        {
            let generation_ok = value.get("generation").is_none_or(|g| g.as_u64().is_some());
            let attempt_ok = value
                .get("attempt_id")
                .is_none_or(|a| a.as_str().is_some_and(|s| !s.is_empty()));
            if generation_ok && attempt_ok {
                return ImportReply::Delivered;
            }
        }
        return ImportReply::Retry;
    }
    if (200..300).contains(&response.status) {
        return ImportReply::Retry;
    }
    let Some((sentence, reason)) = nonempty(&value, "error").zip(nonempty(&value, "reason_code"))
    else {
        return ImportReply::Refused {
            sentence: message("windows.ui.yourjournalcouldntacceptthis", &[]),
            retryable: false,
            clear_saved: false,
        };
    };
    let retryable = matches!(
        reason.as_str(),
        "import_queue_unreachable"
            | "import_metadata_failed"
            | "import_not_found"
            | "import_preview_required"
    ) && value["kind"] != "client_item_conflict";
    ImportReply::Refused {
        sentence,
        retryable,
        clear_saved: retryable && reason == "import_not_found",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn answer(status: u16, body: &str) -> HttpResponse {
        HttpResponse {
            status,
            headers: Vec::new(),
            body: body.as_bytes().into(),
        }
    }

    #[test]
    fn only_complete_custody_proofs_release_an_item() {
        for replay in [false, true] {
            let body = format!(
                r#"{{"status":"ok","task_id":"task","import_id":"item","replay":{replay}}}"#
            );
            assert_eq!(
                classify(&answer(200, &body), false, Some("item")),
                ImportReply::Delivered
            );
            assert_eq!(
                classify(&answer(200, &body), false, Some("another")),
                ImportReply::Retry
            );
        }
        for body in [
            r#"{"status":"ok"}"#,
            r#"{"status":"ok","task_id":"","import_id":"item"}"#,
            r#"{"status":"ok","task_id":"t","import_id":"item","generation":-1}"#,
            "not json",
        ] {
            assert_eq!(
                classify(&answer(200, body), false, Some("item")),
                ImportReply::Retry
            );
        }
        for save in [false, true] {
            assert_eq!(
                classify(
                    &answer(
                        400,
                        r#"{"reason_code":"invalid_operation_for_state","kind":"already_imported"}"#
                    ),
                    save,
                    None
                ),
                ImportReply::Delivered
            );
            assert!(matches!(
                classify(
                    &answer(
                        400,
                        r#"{"reason_code":"invalid_operation_for_state","kind":"already_running"}"#
                    ),
                    save,
                    None
                ),
                ImportReply::Refused { .. }
            ));
        }
    }

    #[test]
    fn retry_is_limited_to_known_transient_refusals() {
        for reason in [
            "import_queue_unreachable",
            "import_metadata_failed",
            "import_not_found",
            "import_preview_required",
        ] {
            let body = serde_json::json!({"error":"held", "reason_code":reason}).to_string();
            assert_eq!(
                classify(&answer(400, &body), true, None),
                ImportReply::Refused {
                    sentence: "held".into(),
                    retryable: true,
                    clear_saved: reason == "import_not_found"
                }
            );
        }
        for body in [
            "not json",
            r#"{"error":"held"}"#,
            r#"{"error":"held","reason_code":"unknown"}"#,
            r#"{"error":"held","reason_code":"import_not_found","kind":"client_item_conflict"}"#,
        ] {
            assert!(matches!(
                classify(&answer(400, body), true, None),
                ImportReply::Refused {
                    retryable: false,
                    ..
                }
            ));
        }
    }

    #[test]
    fn multipart_has_bounded_framing_and_save_attribution() {
        let file = ImportFile {
            id: "stable-id",
            name: "a\r\n\".pdf",
            content_type: "application/pdf",
            path: Path::new("held"),
            handoff: None,
        };
        let multipart = MultipartFile::new(&file, 1_000_000_000);
        assert_eq!(
            multipart.length,
            1_000_000_000 + multipart.prefix.len() as u64 + multipart.suffix.len() as u64
        );
        assert!(multipart.prefix.len() < 1024);
        let prefix = String::from_utf8(multipart.prefix).unwrap();
        assert!(prefix.contains("name=\"imported_via\"\r\n\r\nwindows\r\n"));
        assert!(prefix.contains("name=\"client_item_id\"\r\n\r\nstable-id\r\n"));
        assert!(prefix.contains("filename=\"a.pdf\""));
        assert_eq!(
            String::from_utf8(multipart.suffix).unwrap(),
            "\r\n------solstoneimportstable-id--\r\n"
        );
    }

    #[test]
    fn a_handoff_sends_its_area_and_whole_context_after_the_file() {
        let handoff = Handoff {
            area: "apple_health".into(),
            context: serde_json::from_str(
                r#"{"setting":"line\r\nbreak","future":{"preserve":true}}"#,
            )
            .unwrap(),
        };
        let file = ImportFile {
            id: "stable-id",
            name: "export.zip",
            content_type: "application/zip",
            path: Path::new("held"),
            handoff: Some(&handoff),
        };
        let multipart = MultipartFile::new(&file, 10);
        assert_eq!(
            multipart.length,
            10 + multipart.prefix.len() as u64 + multipart.suffix.len() as u64
        );
        let suffix = String::from_utf8(multipart.suffix).unwrap();
        let boundary = "------solstoneimportstable-id";
        let parts: Vec<_> = suffix.split(boundary).collect();
        assert_eq!(parts[0], "\r\n");
        assert!(parts[1].ends_with("name=\"area\"\r\n\r\napple_health\r\n"));
        assert!(parts[2].ends_with("name=\"source_hint\"\r\n\r\napple_health\r\n"));
        let context = parts[3]
            .strip_prefix("\r\nContent-Disposition: form-data; name=\"context\"\r\n\r\n")
            .and_then(|v| v.strip_suffix("\r\n"))
            .unwrap();
        assert!(!context.contains('\r') && !context.contains('\n'));
        let sent: serde_json::Map<String, serde_json::Value> =
            serde_json::from_str(context).unwrap();
        assert_eq!(sent, handoff.context);
        assert_eq!(parts[4], "--\r\n");

        let quick = Handoff {
            area: "quick".into(),
            context: Default::default(),
        };
        let file = ImportFile {
            handoff: Some(&quick),
            ..file
        };
        let suffix = String::from_utf8(MultipartFile::new(&file, 10).suffix).unwrap();
        assert!(suffix.contains("name=\"area\"\r\n\r\nquick\r\n"));
        assert!(!suffix.contains("source_hint"));
        assert!(suffix.contains("name=\"context\"\r\n\r\n{}\r\n"));
    }
}
