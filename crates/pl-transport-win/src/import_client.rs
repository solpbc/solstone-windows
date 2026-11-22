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
    ReadyToReview {
        import_id: String,
        reason_code: String,
        kind: String,
    },
    Retry,
    AuthStop,
    Refused {
        sentence: String,
        retryable: bool,
        clear_saved: bool,
        reason_code: Option<String>,
        kind: Option<String>,
    },
}

/// Outcome of reconciling one `ready to review` item via `GET /app/import/api/<id>`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReconcileReply {
    /// Journal moved past pending eligibility (`running`/`success`/`failed`/`unconfirmed`/
    /// `noted`/`unavailable`, `saved_start.active == true`, or `saved_start.state == "consumed"`).
    /// Persist a 24h recent receipt durably before deleting local bytes.
    Confirmed,
    /// `404`: waiting copy expired or was discarded; keep local bytes for `send_again`.
    Expired,
    /// `401`/`403`: halt reconciliation batch and record pairing refusal without deleting bytes.
    AuthStop,
    /// Still awaiting review (`pending` + `eligible`/`unknown`/absent `saved_start`), or
    /// `400`/`429`/`5xx`/malformed JSON: leave item unchanged and preserve local bytes.
    Unchanged,
}

/// True only for explicit confirmation-required refusal kinds.
pub fn is_confirmation_required_kind(kind: &str) -> bool {
    kind == "body_confirmation_required"
        || kind == "confirmation_required"
        || kind.ends_with("_confirmation_required")
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

    pub async fn import_status(&self, import_id: &str) -> Result<ReconcileReply, RouteError> {
        if !observer_model::is_valid_import_id(import_id) {
            return Ok(ReconcileReply::Unchanged);
        }
        let headers = self.proxy_headers(&[]);
        let (response, _) = self
            .ordinary_request(
                OrdinaryRequest::ImportStatusGet,
                Some(import_id),
                &headers,
                &[],
            )
            .await?;
        Ok(classify_reconcile(&response))
    }
}

fn nonempty(value: &serde_json::Value, key: &str) -> Option<String> {
    value
        .get(key)?
        .as_str()
        .filter(|v| !v.is_empty())
        .map(str::to_owned)
}

pub fn classify(response: &HttpResponse, save: bool, expected_import: Option<&str>) -> ImportReply {
    if matches!(response.status, 401 | 403) {
        return ImportReply::AuthStop;
    }
    if response.status >= 500 || response.status == 429 {
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
                reason_code: None,
                kind: None,
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
    let reason = nonempty(&value, "reason_code");
    let kind = nonempty(&value, "kind");
    let sentence = nonempty(&value, "error");

    if !save && response.status == 409 && reason.as_deref() == Some("import_preview_required") {
        let candidate_id = match value.get("import_id") {
            Some(serde_json::Value::String(id)) if !id.is_empty() => Some(id.as_str()),
            Some(serde_json::Value::String(_)) | Some(serde_json::Value::Null) | None => {
                expected_import
            }
            Some(_) => None,
        };
        if let (Some(kind_str), Some(import_id)) = (
            kind.as_deref().filter(|k| is_confirmation_required_kind(k)),
            candidate_id.filter(|id| observer_model::is_valid_import_id(id)),
        ) {
            return ImportReply::ReadyToReview {
                import_id: import_id.to_owned(),
                reason_code: "import_preview_required".to_owned(),
                kind: kind_str.to_owned(),
            };
        }
        let retryable = matches!(kind.as_deref(), Some("source_changed" | "already_running"));
        let clear_saved = kind.as_deref() == Some("source_changed");
        return ImportReply::Refused {
            sentence: sentence
                .unwrap_or_else(|| message("windows.ui.yourjournalcouldntacceptthis", &[])),
            retryable,
            clear_saved,
            reason_code: reason,
            kind,
        };
    }

    let Some((sentence, reason)) = sentence.zip(reason) else {
        return ImportReply::Refused {
            sentence: message("windows.ui.yourjournalcouldntacceptthis", &[]),
            retryable: false,
            clear_saved: false,
            reason_code: None,
            kind,
        };
    };
    let retryable = matches!(
        reason.as_str(),
        "import_queue_unreachable" | "import_metadata_failed" | "import_not_found"
    ) && kind.as_deref() != Some("client_item_conflict");
    ImportReply::Refused {
        sentence,
        retryable,
        clear_saved: retryable && reason == "import_not_found",
        reason_code: Some(reason),
        kind,
    }
}

/// Classify `GET /app/import/api/<id>` for safe reconciliation of a `ready to review` item.
pub fn classify_reconcile(response: &HttpResponse) -> ReconcileReply {
    if matches!(response.status, 401 | 403) {
        return ReconcileReply::AuthStop;
    }
    if response.status == 404 {
        return ReconcileReply::Expired;
    }
    if response.status != 200 {
        return ReconcileReply::Unchanged;
    }
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(&response.body) else {
        return ReconcileReply::Unchanged;
    };
    let Some(object) = value.as_object() else {
        return ReconcileReply::Unchanged;
    };
    let status = object.get("status").and_then(serde_json::Value::as_str);
    let saved_start = object
        .get("saved_start")
        .and_then(serde_json::Value::as_object);
    let active = saved_start
        .and_then(|s| s.get("active"))
        .and_then(serde_json::Value::as_bool)
        == Some(true);
    let saved_state = saved_start
        .and_then(|s| s.get("state"))
        .and_then(serde_json::Value::as_str);
    if matches!(
        status,
        Some("running" | "success" | "failed" | "unconfirmed" | "noted" | "unavailable")
    ) || active
        || saved_state == Some("consumed")
    {
        ReconcileReply::Confirmed
    } else {
        ReconcileReply::Unchanged
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
        ] {
            let body = serde_json::json!({"error":"held", "reason_code":reason}).to_string();
            assert_eq!(
                classify(&answer(400, &body), true, None),
                ImportReply::Refused {
                    sentence: "held".into(),
                    retryable: true,
                    clear_saved: reason == "import_not_found",
                    reason_code: Some(reason.into()),
                    kind: None,
                }
            );
        }
        for body in [
            "not json",
            r#"{"error":"held"}"#,
            r#"{"error":"held","reason_code":"unknown"}"#,
            r#"{"error":"held","reason_code":"import_preview_required"}"#,
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
    fn preview_required_409_enters_ready_to_review_only_for_confirmation_kinds_with_valid_ascii_id()
    {
        for kind in [
            "body_confirmation_required",
            "confirmation_required",
            "future_source_confirmation_required",
        ] {
            let with_response_id = serde_json::json!({
                "error": "preview required",
                "reason_code": "import_preview_required",
                "kind": kind,
                "import_id": "20261122_143059",
            })
            .to_string();
            assert_eq!(
                classify(
                    &answer(409, &with_response_id),
                    false,
                    Some("20261122_000001")
                ),
                ImportReply::ReadyToReview {
                    import_id: "20261122_143059".into(),
                    reason_code: "import_preview_required".into(),
                    kind: kind.into(),
                }
            );

            // Falls back to persisted `/save` timestamp when response omits `import_id`.
            let fallback_id = serde_json::json!({
                "error": "preview required",
                "reason_code": "import_preview_required",
                "kind": kind,
            })
            .to_string();
            assert_eq!(
                classify(&answer(409, &fallback_id), false, Some("20261122_143059")),
                ImportReply::ReadyToReview {
                    import_id: "20261122_143059".into(),
                    reason_code: "import_preview_required".into(),
                    kind: kind.into(),
                }
            );
        }

        // Non-retryable 409 preview errors never enter ready_to_review.
        for kind in [
            "plan_failed",
            "nothing_to_import",
            "invalid_request",
            "not_found",
            "unknown_kind",
        ] {
            let body = serde_json::json!({
                "error": "could not plan",
                "reason_code": "import_preview_required",
                "kind": kind,
                "import_id": "20261122_143059",
            })
            .to_string();
            assert_eq!(
                classify(&answer(409, &body), false, Some("20261122_143059")),
                ImportReply::Refused {
                    sentence: "could not plan".into(),
                    retryable: false,
                    clear_saved: false,
                    reason_code: Some("import_preview_required".into()),
                    kind: Some(kind.into()),
                },
                "kind={kind}"
            );
        }

        // source_changed and already_running route to needs attention (never ready_to_review).
        let source_changed = serde_json::json!({
            "error": "source changed",
            "reason_code": "import_preview_required",
            "kind": "source_changed",
            "import_id": "20261122_143059",
        })
        .to_string();
        assert_eq!(
            classify(
                &answer(409, &source_changed),
                false,
                Some("20261122_143059")
            ),
            ImportReply::Refused {
                sentence: "source changed".into(),
                retryable: true,
                clear_saved: true,
                reason_code: Some("import_preview_required".into()),
                kind: Some("source_changed".into()),
            }
        );

        let already_running = serde_json::json!({
            "error": "already running",
            "reason_code": "import_preview_required",
            "kind": "already_running",
            "import_id": "20261122_143059",
        })
        .to_string();
        assert_eq!(
            classify(
                &answer(409, &already_running),
                false,
                Some("20261122_143059")
            ),
            ImportReply::Refused {
                sentence: "already running".into(),
                retryable: true,
                clear_saved: false,
                reason_code: Some("import_preview_required".into()),
                kind: Some("already_running".into()),
            }
        );

        // Absent kind or invalid/Unicode import_id route to non-retryable needs attention.
        for (body, expected) in [
            (
                r#"{"error":"missing kind","reason_code":"import_preview_required","import_id":"20261122_143059"}"#,
                Some("20261122_143059"),
            ),
            (
                r#"{"error":"bad id","reason_code":"import_preview_required","kind":"body_confirmation_required","import_id":"../escape"}"#,
                Some("20261122_143059"),
            ),
            (
                r#"{"error":"unicode id","reason_code":"import_preview_required","kind":"body_confirmation_required","import_id":"\u0662\u0660\u0662\u0666\u0661\u0661\u0662\u0662_\u0661\u0664\u0663\u0660\u0665\u0669"}"#,
                Some("20261122_143059"),
            ),
            (
                r#"{"error":"no id","reason_code":"import_preview_required","kind":"body_confirmation_required"}"#,
                Some("not-a-timestamp"),
            ),
            (
                r#"{"error":"no id","reason_code":"import_preview_required","kind":"body_confirmation_required"}"#,
                None,
            ),
        ] {
            assert!(
                matches!(
                    classify(&answer(409, body), false, expected),
                    ImportReply::Refused {
                        retryable: false,
                        ..
                    }
                ),
                "body={body}"
            );
        }
    }

    #[test]
    fn reconcile_classification_deletes_only_on_explicit_confirmation_proofs() {
        for status in [
            "running",
            "success",
            "failed",
            "unconfirmed",
            "noted",
            "unavailable",
        ] {
            let body = serde_json::json!({"status": status}).to_string();
            assert_eq!(
                classify_reconcile(&answer(200, &body)),
                ReconcileReply::Confirmed,
                "status={status}"
            );
        }
        assert_eq!(
            classify_reconcile(&answer(
                200,
                r#"{"status":"pending","saved_start":{"active":true,"state":"eligible"}}"#
            )),
            ReconcileReply::Confirmed
        );
        assert_eq!(
            classify_reconcile(&answer(
                200,
                r#"{"status":"pending","saved_start":{"active":false,"state":"consumed"}}"#
            )),
            ReconcileReply::Confirmed
        );

        // Must preserve local bytes on pending + eligible/unknown/omitted saved_start,
        // 400, 429, 5xx, and malformed JSON.
        for (code, body) in [
            (
                200,
                r#"{"status":"pending","saved_start":{"active":false,"state":"eligible"}}"#,
            ),
            (
                200,
                r#"{"status":"pending","saved_start":{"active":false,"state":"unknown"}}"#,
            ),
            (200, r#"{"status":"pending"}"#),
            (200, r#"{"status":"pending","saved_start":null}"#),
            (200, "not json"),
            (200, "[]"),
            (400, r#"{"error":"bad"}"#),
            (429, r#"{"error":"slow down"}"#),
            (500, "internal error"),
            (502, "journal unreachable"),
            (503, "busy"),
        ] {
            assert_eq!(
                classify_reconcile(&answer(code, body)),
                ReconcileReply::Unchanged,
                "code={code} body={body}"
            );
        }

        assert_eq!(
            classify_reconcile(&answer(404, r#"{"error":"not found"}"#)),
            ReconcileReply::Expired
        );
        assert_eq!(
            classify_reconcile(&answer(401, "")),
            ReconcileReply::AuthStop
        );
        assert_eq!(
            classify_reconcile(&answer(403, "")),
            ReconcileReply::AuthStop
        );
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
