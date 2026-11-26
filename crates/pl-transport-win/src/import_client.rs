// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! The linked-device import contract: streamed save, then idempotent start.

use observer_model::i18n::message;

use std::fs::File;
use std::io;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use spl_core::http::HttpResponse;
use tokio::io::AsyncReadExt;

use crate::ordinary_request::OrdinaryRequest;
use crate::streamed_request::{self, UploadSession};
use crate::{ObserverClient, RouteError, TransportError};

pub use crate::streamed_request::{REQUEST_BODY_LIMIT, UPLOAD_BRIDGE_MAX_BODY_BYTES};
pub const MAX_IMPORT_FILE_BYTES: u64 = 4 * 1024 * 1024 * 1024;
pub const RESUMABLE_CAPABILITY: &str = "resumable_import_uploads";
pub const RESUMABLE_RETRY_LADDER_SECS: [u64; 6] = [1, 2, 5, 10, 30, 60];

/// Limits advertised by the journal on `GET /app/import/api/sources` when
/// `resumable_import_uploads` is enabled.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ResumableConfig {
    pub threshold_bytes: u64,
    pub chunk_bytes: u64,
    pub max_file_bytes: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResumableProbe {
    Supported(ResumableConfig),
    Unsupported,
    AuthStop,
    Retry,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UploadOpen {
    pub upload_id: String,
    pub next_offset: u64,
    pub total_bytes: u64,
    pub chunk_bytes: u64,
    pub expires_at: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UploadCreateReply {
    Open(UploadOpen),
    AlreadyStaged(SavedImport),
    AlreadyImported {
        import_id: Option<String>,
    },
    /// Older journal without `/app/import/api/uploads` (`404` or `405`).
    Unsupported,
    InsufficientSpace {
        sentence: String,
        required_bytes: Option<u64>,
        available_bytes: Option<u64>,
    },
    AuthStop,
    Retry,
    Refused {
        sentence: String,
        retryable: bool,
        reason_code: Option<String>,
        kind: Option<String>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UploadChunkReply {
    Interrupted,
    Accepted {
        next_offset: u64,
        total_bytes: u64,
        complete: bool,
        expires_at: Option<String>,
    },
    OffsetMismatch {
        next_offset: Option<u64>,
    },
    ChunkChecksumMismatch,
    UploadNotFound,
    InsufficientSpace {
        sentence: String,
        required_bytes: Option<u64>,
        available_bytes: Option<u64>,
    },
    AuthStop,
    Retry,
    Refused {
        sentence: String,
        retryable: bool,
        reason_code: Option<String>,
        kind: Option<String>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UploadStatusReply {
    Open {
        upload_id: String,
        next_offset: u64,
        total_bytes: u64,
        chunk_bytes: u64,
        phase: Option<String>,
        expires_at: Option<String>,
    },
    UploadNotFound,
    AuthStop,
    Retry,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UploadCommitReply {
    Saved(SavedImport),
    Incomplete {
        next_offset: Option<u64>,
    },
    WholeFileChecksumMismatch {
        sentence: String,
    },
    UploadNotFound,
    InsufficientSpace {
        sentence: String,
        required_bytes: Option<u64>,
        available_bytes: Option<u64>,
    },
    AuthStop,
    Retry,
    Refused {
        sentence: String,
        retryable: bool,
        reason_code: Option<String>,
        kind: Option<String>,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UploadAbortReply {
    Deleted,
    NotFound,
    AuthStop,
    Retry,
}

/// Retry backoff ladder (`1s -> 2s -> 5s -> 10s -> 30s -> 60s`, +/-20% jitter).
#[derive(Clone, Debug, Default)]
pub struct RetryLadder {
    step: usize,
}

impl RetryLadder {
    pub fn new() -> Self {
        Self { step: 0 }
    }

    pub fn reset(&mut self) {
        self.step = 0;
    }

    pub fn step(&self) -> usize {
        self.step
    }

    /// Compute the next backoff duration using deterministic jitter in `[-1.0, 1.0]`.
    pub fn next_delay_with_jitter(&mut self, unit_signed: f64) -> Duration {
        let base_secs =
            RESUMABLE_RETRY_LADDER_SECS[self.step.min(RESUMABLE_RETRY_LADDER_SECS.len() - 1)];
        if self.step + 1 < RESUMABLE_RETRY_LADDER_SECS.len() {
            self.step += 1;
        }
        let clamped = unit_signed.clamp(-1.0, 1.0);
        let factor = 1.0 + 0.2 * clamped;
        let millis = ((base_secs as f64) * 1000.0 * factor).round().max(1.0) as u64;
        Duration::from_millis(millis)
    }

    pub fn next_delay(&mut self) -> Duration {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(0);
        // Mix step and sub-second clock into a smooth jitter in [-1.0, 1.0].
        let mixed = nanos.wrapping_mul(2_654_435_761) ^ ((self.step as u32).wrapping_mul(40_503));
        let unit = ((mixed % 10_001) as f64 / 5_000.0) - 1.0;
        self.next_delay_with_jitter(unit)
    }
}

pub fn is_valid_upload_id(id: &str) -> bool {
    streamed_request::is_valid_upload_id(id)
}

pub fn is_valid_sha256_hex(hex: &str) -> bool {
    hex.len() == 64 && hex.bytes().all(|b| b.is_ascii_hexdigit())
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    observer_log::sha256_hex(bytes)
}

/// Stream-hash a stored custody file with bounded 64 KiB memory, returning
/// `(lowercase_sha256_hex, exact_byte_len)`.
pub fn hash_file_sha256(path: &Path) -> io::Result<(String, u64)> {
    let file = File::open(path)?;
    observer_log::hash_reader_sha256(file)
}

/// Read one exact chunk slice from disk at `offset` (`len` bytes).
pub fn read_file_chunk(path: &Path, offset: u64, len: usize) -> io::Result<Vec<u8>> {
    use std::io::{Read as _, Seek as _, SeekFrom};
    let mut file = File::open(path)?;
    file.seek(SeekFrom::Start(offset))?;
    let mut buf = vec![0u8; len];
    file.read_exact(&mut buf)?;
    Ok(buf)
}

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

    /// Probe whether the paired journal advertises `resumable_import_uploads`
    /// and read its `threshold_bytes`, `chunk_bytes`, and `max_file_bytes` from
    /// `GET /app/import/api/sources`.
    pub async fn probe_resumable_import(&self) -> Result<ResumableProbe, RouteError> {
        let caps = match self.system_capabilities().await {
            Ok(caps) => caps,
            Err(TransportError::Rejected {
                status: 401 | 403, ..
            }) => {
                return Ok(ResumableProbe::AuthStop);
            }
            Err(error) => return Err(RouteError::Transport(error)),
        };
        if !caps.iter().any(|cap| cap == RESUMABLE_CAPABILITY) {
            return Ok(ResumableProbe::Unsupported);
        }
        let headers = self.proxy_headers(&[]);
        let (response, _) = self
            .ordinary_request(OrdinaryRequest::ImportSourcesGet, None, &headers, &[])
            .await?;
        Ok(classify_sources_resumable(&response))
    }

    /// Best-effort abort of an open resumable upload when no upload session is active.
    pub async fn abort_upload(&self, upload_id: &str) -> Result<UploadAbortReply, RouteError> {
        if !is_valid_upload_id(upload_id) {
            return Ok(UploadAbortReply::NotFound);
        }
        let headers = self.proxy_headers(&[]);
        let (response, _) = self
            .ordinary_request(
                OrdinaryRequest::ImportUploadAbortDelete,
                Some(upload_id),
                &headers,
                &[],
            )
            .await?;
        Ok(classify_upload_abort(&response))
    }
}

/// Reusable single-carrier session for `/app/import/api/uploads*`.
pub struct ResumableUploadSession {
    session: UploadSession,
}

impl ResumableUploadSession {
    pub async fn open(client: Arc<ObserverClient>) -> Result<Self, RouteError> {
        Ok(Self {
            session: UploadSession::open(client).await?,
        })
    }

    pub fn is_alive(&self) -> bool {
        self.session.is_alive()
    }

    pub async fn create_upload(
        &mut self,
        file: &ImportFile<'_>,
        total_bytes: u64,
        sha256: &str,
    ) -> Result<UploadCreateReply, RouteError> {
        let body = build_upload_create_body(file, total_bytes, sha256)?;
        let headers = [("Content-Type".into(), "application/json".into())];
        let response = self
            .session
            .send(
                "POST",
                "/app/import/api/uploads",
                &headers,
                body.as_slice(),
                body.len() as u64,
            )
            .await?;
        Ok(classify_upload_create(&response))
    }

    pub async fn put_chunk(
        &mut self,
        upload_id: &str,
        offset: u64,
        bytes: &[u8],
    ) -> Result<UploadChunkReply, RouteError> {
        if !is_valid_upload_id(upload_id) || bytes.is_empty() {
            return Err(RouteError::Transport(
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "invalid chunk upload arguments",
                )
                .into(),
            ));
        }
        let chunk_sha = sha256_hex(bytes);
        let target = format!("/app/import/api/uploads/{upload_id}?offset={offset}");
        let headers = [
            ("Content-Type".into(), "application/octet-stream".into()),
            ("X-Chunk-Sha256".into(), chunk_sha),
        ];
        let started = tokio::time::Instant::now();
        let response = self
            .session
            .send("PUT", &target, &headers, bytes, bytes.len() as u64)
            .await;
        classify_put_exchange(response, started.elapsed(), upload_id)
    }

    pub async fn get_status(&mut self, upload_id: &str) -> Result<UploadStatusReply, RouteError> {
        if !is_valid_upload_id(upload_id) {
            return Ok(UploadStatusReply::UploadNotFound);
        }
        let target = format!("/app/import/api/uploads/{upload_id}");
        let response = self
            .session
            .send("GET", &target, &[], [].as_slice(), 0)
            .await?;
        Ok(classify_upload_status(&response))
    }

    pub async fn commit_upload(
        &mut self,
        upload_id: &str,
        sha256: &str,
    ) -> Result<UploadCommitReply, RouteError> {
        if !is_valid_upload_id(upload_id) {
            return Ok(UploadCommitReply::UploadNotFound);
        }
        let body = serde_json::to_vec(&serde_json::json!({ "sha256": sha256 }))
            .map_err(TransportError::from)?;
        let target = format!("/app/import/api/uploads/{upload_id}/commit");
        let headers = [("Content-Type".into(), "application/json".into())];
        let response = self
            .session
            .send(
                "POST",
                &target,
                &headers,
                body.as_slice(),
                body.len() as u64,
            )
            .await?;
        Ok(classify_upload_commit(&response))
    }

    pub async fn abort_upload(&mut self, upload_id: &str) -> Result<UploadAbortReply, RouteError> {
        if !is_valid_upload_id(upload_id) {
            return Ok(UploadAbortReply::NotFound);
        }
        let target = format!("/app/import/api/uploads/{upload_id}");
        let response = self
            .session
            .send("DELETE", &target, &[], [].as_slice(), 0)
            .await?;
        Ok(classify_upload_abort(&response))
    }

    pub async fn close(self) {
        self.session.close().await;
    }
}

pub fn build_upload_create_body(
    file: &ImportFile<'_>,
    total_bytes: u64,
    sha256: &str,
) -> Result<Vec<u8>, RouteError> {
    let mut object = serde_json::Map::new();
    object.insert(
        "client_item_id".into(),
        serde_json::Value::String(file.id.into()),
    );
    object.insert(
        "filename".into(),
        serde_json::Value::String(filename(file.name)),
    );
    object.insert(
        "mime_type".into(),
        serde_json::Value::String(file.content_type.into()),
    );
    object.insert("total_bytes".into(), serde_json::Value::from(total_bytes));
    if is_valid_sha256_hex(sha256) {
        object.insert(
            "sha256".into(),
            serde_json::Value::String(sha256.to_ascii_lowercase()),
        );
    }
    object.insert(
        "imported_via".into(),
        serde_json::Value::String("windows".into()),
    );
    object.insert("device_noun".into(), serde_json::Value::String("PC".into()));
    if let Some(handoff) = file.handoff {
        object.insert(
            "area".into(),
            serde_json::Value::String(handoff.area.clone()),
        );
        if !matches!(handoff.area.as_str(), "quick" | "recording") {
            object.insert(
                "source_hint".into(),
                serde_json::Value::String(handoff.area.clone()),
            );
        }
        object.insert(
            "context".into(),
            serde_json::Value::Object(handoff.context.clone()),
        );
    }
    serde_json::to_vec(&serde_json::Value::Object(object))
        .map_err(|e| RouteError::Transport(e.into()))
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

pub fn classify_sources_resumable(response: &HttpResponse) -> ResumableProbe {
    if matches!(response.status, 401 | 403) {
        return ResumableProbe::AuthStop;
    }
    if matches!(response.status, 404 | 405) {
        return ResumableProbe::Unsupported;
    }
    if response.status >= 500 || response.status == 429 {
        return ResumableProbe::Retry;
    }
    if response.status != 200 {
        return ResumableProbe::Unsupported;
    }
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(&response.body) else {
        return ResumableProbe::Retry;
    };
    let Some(resumable) = value.get("resumable") else {
        return ResumableProbe::Unsupported;
    };
    if resumable.is_null() {
        return ResumableProbe::Unsupported;
    }
    let Some(obj) = resumable.as_object() else {
        return ResumableProbe::Retry;
    };
    let threshold_bytes = obj
        .get("threshold_bytes")
        .and_then(serde_json::Value::as_u64);
    let chunk_bytes = obj.get("chunk_bytes").and_then(serde_json::Value::as_u64);
    let max_file_bytes = obj
        .get("max_file_bytes")
        .and_then(serde_json::Value::as_u64);
    match (threshold_bytes, chunk_bytes, max_file_bytes) {
        (Some(threshold), Some(chunk), Some(max_file))
            if threshold > 0
                && chunk > 0
                && chunk <= UPLOAD_BRIDGE_MAX_BODY_BYTES as u64
                && max_file >= threshold =>
        {
            ResumableProbe::Supported(ResumableConfig {
                threshold_bytes: threshold,
                chunk_bytes: chunk,
                max_file_bytes: max_file,
            })
        }
        _ => ResumableProbe::Retry,
    }
}

fn parse_saved_import(value: &serde_json::Value) -> Option<SavedImport> {
    if value.get("status").and_then(serde_json::Value::as_str) != Some("staged") {
        return None;
    }
    let path = nonempty(value, "path")?;
    let timestamp = nonempty(value, "timestamp")?;
    Some(SavedImport { path, timestamp })
}

fn parse_space_fields(value: Option<&serde_json::Value>) -> (String, Option<u64>, Option<u64>) {
    let sentence = value
        .and_then(|v| nonempty(v, "error"))
        .unwrap_or_else(|| message("windows.ui.yourjournalcouldntacceptthis", &[]));
    let required_bytes = value
        .and_then(|v| v.get("required_bytes"))
        .and_then(serde_json::Value::as_u64);
    let available_bytes = value
        .and_then(|v| v.get("available_bytes"))
        .and_then(serde_json::Value::as_u64);
    (sentence, required_bytes, available_bytes)
}

pub fn classify_upload_create(response: &HttpResponse) -> UploadCreateReply {
    if matches!(response.status, 401 | 403) {
        return UploadCreateReply::AuthStop;
    }
    let parsed = serde_json::from_slice::<serde_json::Value>(&response.body).ok();
    if response.status == 507 {
        let (sentence, required_bytes, available_bytes) = parse_space_fields(parsed.as_ref());
        return UploadCreateReply::InsufficientSpace {
            sentence,
            required_bytes,
            available_bytes,
        };
    }
    if response.status >= 500 || response.status == 429 {
        return UploadCreateReply::Retry;
    }
    if matches!(response.status, 404 | 405) {
        let reason = parsed.as_ref().and_then(|v| nonempty(v, "reason_code"));
        if reason.as_deref() != Some("upload_not_found") {
            return UploadCreateReply::Unsupported;
        }
    }
    if matches!(response.status, 200 | 201) {
        let Some(value) = parsed else {
            return UploadCreateReply::Retry;
        };
        match value.get("status").and_then(serde_json::Value::as_str) {
            Some("open") => {
                let upload_id = nonempty(&value, "upload_id").filter(|id| is_valid_upload_id(id));
                let next_offset = value.get("next_offset").and_then(serde_json::Value::as_u64);
                let total_bytes = value.get("total_bytes").and_then(serde_json::Value::as_u64);
                let chunk_bytes = value.get("chunk_bytes").and_then(serde_json::Value::as_u64);
                if let (Some(upload_id), Some(next_offset), Some(total_bytes), Some(chunk_bytes)) =
                    (upload_id, next_offset, total_bytes, chunk_bytes)
                {
                    if total_bytes > 0
                        && next_offset <= total_bytes
                        && chunk_bytes > 0
                        && chunk_bytes <= UPLOAD_BRIDGE_MAX_BODY_BYTES as u64
                    {
                        return UploadCreateReply::Open(UploadOpen {
                            upload_id,
                            next_offset,
                            total_bytes,
                            chunk_bytes,
                            expires_at: nonempty(&value, "expires_at"),
                        });
                    }
                }
                return UploadCreateReply::Retry;
            }
            Some("already_staged") if response.status == 200 => {
                if let Some(saved) = value.get("save_response").and_then(parse_saved_import) {
                    return UploadCreateReply::AlreadyStaged(saved);
                }
                return UploadCreateReply::Retry;
            }
            Some("already_imported") if response.status == 200 => {
                let import_id = nonempty(&value, "import_id");
                return UploadCreateReply::AlreadyImported { import_id };
            }
            _ => return UploadCreateReply::Retry,
        }
    }
    if (200..300).contains(&response.status) {
        return UploadCreateReply::Retry;
    }
    let reason = parsed.as_ref().and_then(|v| nonempty(v, "reason_code"));
    if response.status == 409 && reason.as_deref() == Some("journal_busy") {
        return UploadCreateReply::Retry;
    }
    let kind = parsed.as_ref().and_then(|v| nonempty(v, "kind"));
    let sentence = parsed
        .as_ref()
        .and_then(|v| nonempty(v, "error"))
        .unwrap_or_else(|| message("windows.ui.yourjournalcouldntacceptthis", &[]));
    UploadCreateReply::Refused {
        sentence,
        retryable: false,
        reason_code: reason,
        kind,
    }
}

fn classify_put_exchange(
    response: Result<HttpResponse, RouteError>,
    elapsed: Duration,
    upload_id: &str,
) -> Result<UploadChunkReply, RouteError> {
    match response {
        Err(RouteError::Transport(TransportError::Io(error)))
            if error.kind() == io::ErrorKind::TimedOut =>
        {
            Ok(UploadChunkReply::Interrupted)
        }
        // The loopback bridge also reports carrier loss as an HTTP 502. A
        // brief redeploy can resume at full size; sustained trouble can
        // justify a smaller piece once the journal confirms no progress.
        Ok(response)
            if response.status == 502
                && response.body == b"journal unreachable"
                && elapsed >= Duration::from_secs(60) =>
        {
            Ok(UploadChunkReply::Interrupted)
        }
        Ok(response) => Ok(classify_upload_chunk_for(&response, Some(upload_id))),
        Err(error) => Err(error),
    }
}

pub fn classify_upload_chunk(response: &HttpResponse) -> UploadChunkReply {
    classify_upload_chunk_for(response, None)
}

fn classify_upload_chunk_for(
    response: &HttpResponse,
    expected_id: Option<&str>,
) -> UploadChunkReply {
    if matches!(response.status, 401 | 403) {
        return UploadChunkReply::AuthStop;
    }
    let parsed = serde_json::from_slice::<serde_json::Value>(&response.body).ok();
    if response.status == 507 {
        let (sentence, required_bytes, available_bytes) = parse_space_fields(parsed.as_ref());
        return UploadChunkReply::InsufficientSpace {
            sentence,
            required_bytes,
            available_bytes,
        };
    }
    if response.status >= 500 || matches!(response.status, 405 | 429) {
        return UploadChunkReply::Retry;
    }
    if response.status == 404 {
        return if parsed
            .as_ref()
            .is_some_and(|body| body["reason_code"] == "upload_not_found")
        {
            UploadChunkReply::UploadNotFound
        } else {
            UploadChunkReply::Retry
        };
    }
    if response.status == 200 {
        let Some(value) = parsed else {
            return UploadChunkReply::Retry;
        };
        let next_offset = value.get("next_offset").and_then(serde_json::Value::as_u64);
        let total_bytes = value.get("total_bytes").and_then(serde_json::Value::as_u64);
        let complete = value.get("complete").and_then(serde_json::Value::as_bool);
        if let (Some(next_offset), Some(total_bytes), Some(complete)) =
            (next_offset, total_bytes, complete)
        {
            let wrong_id = expected_id
                .zip(value.get("upload_id"))
                .is_some_and(|(expected, actual)| actual.as_str() != Some(expected));
            if !wrong_id
                && total_bytes > 0
                && next_offset <= total_bytes
                && complete == (next_offset == total_bytes)
            {
                return UploadChunkReply::Accepted {
                    next_offset,
                    total_bytes,
                    complete,
                    expires_at: nonempty(&value, "expires_at"),
                };
            }
        }
        return UploadChunkReply::Retry;
    }
    if (200..300).contains(&response.status) {
        return UploadChunkReply::Retry;
    }
    let reason = parsed.as_ref().and_then(|v| nonempty(v, "reason_code"));
    if response.status == 409 {
        if reason.as_deref() == Some("journal_busy") {
            return UploadChunkReply::Retry;
        }
        if reason.as_deref() == Some("offset_mismatch") || parsed.is_none() {
            let next_offset = parsed
                .as_ref()
                .and_then(|v| v.get("next_offset"))
                .and_then(serde_json::Value::as_u64);
            return UploadChunkReply::OffsetMismatch { next_offset };
        }
    }
    if response.status == 422 && reason.as_deref() == Some("chunk_checksum_mismatch") {
        return UploadChunkReply::ChunkChecksumMismatch;
    }
    let kind = parsed.as_ref().and_then(|v| nonempty(v, "kind"));
    let sentence = parsed
        .as_ref()
        .and_then(|v| nonempty(v, "error"))
        .unwrap_or_else(|| message("windows.ui.yourjournalcouldntacceptthis", &[]));
    UploadChunkReply::Refused {
        sentence,
        retryable: false,
        reason_code: reason,
        kind,
    }
}

pub fn classify_upload_status(response: &HttpResponse) -> UploadStatusReply {
    if matches!(response.status, 401 | 403) {
        return UploadStatusReply::AuthStop;
    }
    if response.status == 404 {
        return match serde_json::from_slice::<serde_json::Value>(&response.body) {
            Ok(body) if body["reason_code"] == "upload_not_found" => {
                UploadStatusReply::UploadNotFound
            }
            _ => UploadStatusReply::Retry,
        };
    }
    if response.status != 200 {
        return UploadStatusReply::Retry;
    }
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(&response.body) else {
        return UploadStatusReply::Retry;
    };
    if value.get("status").and_then(serde_json::Value::as_str) != Some("open") {
        return UploadStatusReply::Retry;
    }
    let upload_id = nonempty(&value, "upload_id").filter(|id| is_valid_upload_id(id));
    let next_offset = value.get("next_offset").and_then(serde_json::Value::as_u64);
    let total_bytes = value.get("total_bytes").and_then(serde_json::Value::as_u64);
    let chunk_bytes = value.get("chunk_bytes").and_then(serde_json::Value::as_u64);
    match (upload_id, next_offset, total_bytes, chunk_bytes) {
        (Some(upload_id), Some(next_offset), Some(total_bytes), Some(chunk_bytes))
            if total_bytes > 0
                && next_offset <= total_bytes
                && chunk_bytes > 0
                && chunk_bytes <= UPLOAD_BRIDGE_MAX_BODY_BYTES as u64 =>
        {
            UploadStatusReply::Open {
                upload_id,
                next_offset,
                total_bytes,
                chunk_bytes,
                phase: nonempty(&value, "phase"),
                expires_at: nonempty(&value, "expires_at"),
            }
        }
        _ => UploadStatusReply::Retry,
    }
}

pub fn classify_upload_commit(response: &HttpResponse) -> UploadCommitReply {
    if matches!(response.status, 401 | 403) {
        return UploadCommitReply::AuthStop;
    }
    let parsed = serde_json::from_slice::<serde_json::Value>(&response.body).ok();
    if response.status == 507 {
        let (sentence, required_bytes, available_bytes) = parse_space_fields(parsed.as_ref());
        return UploadCommitReply::InsufficientSpace {
            sentence,
            required_bytes,
            available_bytes,
        };
    }
    if response.status >= 500 || matches!(response.status, 405 | 429) {
        return UploadCommitReply::Retry;
    }
    if response.status == 404 {
        return if parsed
            .as_ref()
            .is_some_and(|body| body["reason_code"] == "upload_not_found")
        {
            UploadCommitReply::UploadNotFound
        } else {
            UploadCommitReply::Retry
        };
    }
    if response.status == 200 {
        if let Some(saved) = parsed.as_ref().and_then(parse_saved_import) {
            return UploadCommitReply::Saved(saved);
        }
        return UploadCommitReply::Retry;
    }
    if (200..300).contains(&response.status) {
        return UploadCommitReply::Retry;
    }
    let reason = parsed.as_ref().and_then(|v| nonempty(v, "reason_code"));
    if response.status == 409 {
        if reason.as_deref() == Some("journal_busy") {
            return UploadCommitReply::Retry;
        }
        if reason.as_deref() == Some("incomplete_upload") || parsed.is_none() {
            let next_offset = parsed
                .as_ref()
                .and_then(|v| v.get("next_offset"))
                .and_then(serde_json::Value::as_u64);
            return UploadCommitReply::Incomplete { next_offset };
        }
    }
    if response.status == 422 && reason.as_deref() == Some("whole_file_checksum_mismatch") {
        let sentence = parsed
            .as_ref()
            .and_then(|v| nonempty(v, "error"))
            .unwrap_or_else(|| message("windows.ui.yourjournalcouldntacceptthis", &[]));
        return UploadCommitReply::WholeFileChecksumMismatch { sentence };
    }
    let kind = parsed.as_ref().and_then(|v| nonempty(v, "kind"));
    let sentence = parsed
        .as_ref()
        .and_then(|v| nonempty(v, "error"))
        .unwrap_or_else(|| message("windows.ui.yourjournalcouldntacceptthis", &[]));
    UploadCommitReply::Refused {
        sentence,
        retryable: false,
        reason_code: reason,
        kind,
    }
}

pub fn classify_upload_abort(response: &HttpResponse) -> UploadAbortReply {
    if matches!(response.status, 401 | 403) {
        return UploadAbortReply::AuthStop;
    }
    if matches!(response.status, 200 | 204) {
        return UploadAbortReply::Deleted;
    }
    if response.status == 404 {
        return UploadAbortReply::NotFound;
    }
    UploadAbortReply::Retry
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

    #[test]
    fn sources_resumable_probe_reads_journal_limits_and_treats_404_and_405_as_unsupported() {
        let supported = answer(
            200,
            r#"{"sources":[],"resumable":{"threshold_bytes":16777216,"chunk_bytes":4194304,"max_file_bytes":4294967296}}"#,
        );
        assert_eq!(
            classify_sources_resumable(&supported),
            ResumableProbe::Supported(ResumableConfig {
                threshold_bytes: 16_777_216,
                chunk_bytes: 4_194_304,
                max_file_bytes: 4_294_967_296,
            })
        );
        // Custom advertised threshold/chunk sizes are respected rather than hard-coded.
        let custom = answer(
            200,
            r#"{"resumable":{"threshold_bytes":8388608,"chunk_bytes":2097152,"max_file_bytes":1073741824}}"#,
        );
        assert_eq!(
            classify_sources_resumable(&custom),
            ResumableProbe::Supported(ResumableConfig {
                threshold_bytes: 8_388_608,
                chunk_bytes: 2_097_152,
                max_file_bytes: 1_073_741_824,
            })
        );
        assert_eq!(
            classify_sources_resumable(&answer(200, r#"{"sources":[]}"#)),
            ResumableProbe::Unsupported
        );
        assert_eq!(
            classify_sources_resumable(&answer(404, "")),
            ResumableProbe::Unsupported
        );
        assert_eq!(
            classify_sources_resumable(&answer(405, "")),
            ResumableProbe::Unsupported
        );
        assert_eq!(
            classify_sources_resumable(&answer(401, "")),
            ResumableProbe::AuthStop
        );
        assert_eq!(
            classify_sources_resumable(&answer(503, "")),
            ResumableProbe::Retry
        );
    }

    #[test]
    fn upload_create_classifies_open_dedup_fallback_and_space_faults() {
        assert_eq!(
            classify_upload_create(&answer(
                201,
                r#"{"upload_id":"550e8400-e29b-41d4-a716-446655440000","client_item_id":"id-1","status":"open","next_offset":0,"total_bytes":33554432,"chunk_bytes":4194304,"expires_at":"2026-12-01T00:00:00Z"}"#,
            )),
            UploadCreateReply::Open(UploadOpen {
                upload_id: "550e8400-e29b-41d4-a716-446655440000".into(),
                next_offset: 0,
                total_bytes: 33_554_432,
                chunk_bytes: 4_194_304,
                expires_at: Some("2026-12-01T00:00:00Z".into()),
            })
        );
        assert_eq!(
            classify_upload_create(&answer(
                200,
                r#"{"status":"already_staged","replay":true,"save_response":{"status":"staged","path":"/imports/20261125_120000/a.zip","timestamp":"20261125_120000"}}"#,
            )),
            UploadCreateReply::AlreadyStaged(SavedImport {
                path: "/imports/20261125_120000/a.zip".into(),
                timestamp: "20261125_120000".into(),
            })
        );
        assert_eq!(
            classify_upload_create(&answer(
                200,
                r#"{"status":"already_imported","import_id":"20261125_120000","reason_code":"invalid_operation_for_state","kind":"already_imported"}"#,
            )),
            UploadCreateReply::AlreadyImported {
                import_id: Some("20261125_120000".into()),
            }
        );
        // Unprobed older journal returns 405 (or 404); both fall back identically.
        assert_eq!(
            classify_upload_create(&answer(404, "")),
            UploadCreateReply::Unsupported
        );
        assert_eq!(
            classify_upload_create(&answer(405, "")),
            UploadCreateReply::Unsupported
        );
        assert_eq!(
            classify_upload_create(&answer(
                507,
                r#"{"error":"not enough room","reason_code":"insufficient_journal_space","detail":"low space","required_bytes":4294967296,"available_bytes":100}"#,
            )),
            UploadCreateReply::InsufficientSpace {
                sentence: "not enough room".into(),
                required_bytes: Some(4_294_967_296),
                available_bytes: Some(100),
            }
        );
        assert_eq!(
            classify_upload_create(&answer(
                409,
                r#"{"error":"busy","reason_code":"journal_busy","detail":"busy"}"#,
            )),
            UploadCreateReply::Retry
        );
    }

    #[test]
    fn interrupted_puts_require_journal_status_before_size_recovery() {
        assert!(matches!(
            classify_put_exchange(
                Err(RouteError::Transport(TransportError::Io(
                    io::ErrorKind::TimedOut.into()
                ))),
                Duration::ZERO,
                "upload"
            ),
            Ok(UploadChunkReply::Interrupted)
        ));
        for (code, seconds) in [(502, 2), (502, 120), (503, 120), (429, 120)] {
            assert!(matches!(
                classify_put_exchange(Ok(answer(code, "")), Duration::from_secs(seconds), "upload"),
                Ok(UploadChunkReply::Retry)
            ));
        }
        assert!(matches!(
            classify_put_exchange(
                Ok(answer(502, "journal unreachable")),
                Duration::from_secs(120),
                "upload"
            ),
            Ok(UploadChunkReply::Interrupted)
        ));
        assert_eq!(
            classify_upload_chunk(&answer(404, "")),
            UploadChunkReply::Retry
        );
        assert_eq!(
            classify_upload_commit(&answer(404, "")),
            UploadCommitReply::Retry
        );
        assert_eq!(
            classify_upload_status(&answer(404, "")),
            UploadStatusReply::Retry
        );
        assert_eq!(
            classify_upload_status(&answer(404, r#"{"reason_code":"upload_not_found"}"#)),
            UploadStatusReply::UploadNotFound
        );
    }

    #[test]
    fn upload_chunk_and_commit_classifiers_cover_resync_checksum_expiry_and_space() {
        for body in [
            r#"{"upload_id":"other","next_offset":8,"total_bytes":16,"complete":false}"#,
            r#"{"upload_id":"upload","next_offset":8,"total_bytes":16,"complete":true}"#,
        ] {
            assert_eq!(
                classify_upload_chunk_for(&answer(200, body), Some("upload")),
                UploadChunkReply::Retry
            );
        }
        assert_eq!(
            classify_upload_chunk(&answer(
                200,
                r#"{"upload_id":"u1","next_offset":4194304,"total_bytes":16777216,"complete":false,"expires_at":"2026-12-01T00:00:00Z"}"#,
            )),
            UploadChunkReply::Accepted {
                next_offset: 4_194_304,
                total_bytes: 16_777_216,
                complete: false,
                expires_at: Some("2026-12-01T00:00:00Z".into()),
            }
        );
        assert_eq!(
            classify_upload_chunk(&answer(
                409,
                r#"{"error":"mismatch","reason_code":"offset_mismatch","detail":"bad","next_offset":8388608}"#,
            )),
            UploadChunkReply::OffsetMismatch {
                next_offset: Some(8_388_608),
            }
        );
        assert_eq!(
            classify_upload_chunk(&answer(409, "")),
            UploadChunkReply::OffsetMismatch { next_offset: None }
        );
        assert_eq!(
            classify_upload_chunk(&answer(
                422,
                r#"{"error":"bad chunk","reason_code":"chunk_checksum_mismatch","detail":"mismatch"}"#,
            )),
            UploadChunkReply::ChunkChecksumMismatch
        );
        assert_eq!(
            classify_upload_chunk(&answer(
                404,
                r#"{"error":"gone","reason_code":"upload_not_found","detail":"expired"}"#,
            )),
            UploadChunkReply::UploadNotFound
        );
        assert_eq!(
            classify_upload_commit(&answer(
                200,
                r#"{"status":"staged","path":"/imports/20261125_120000/big.zip","timestamp":"20261125_120000"}"#,
            )),
            UploadCommitReply::Saved(SavedImport {
                path: "/imports/20261125_120000/big.zip".into(),
                timestamp: "20261125_120000".into(),
            })
        );
        assert_eq!(
            classify_upload_commit(&answer(
                422,
                r#"{"error":"file changed","reason_code":"whole_file_checksum_mismatch","detail":"bad sha"}"#,
            )),
            UploadCommitReply::WholeFileChecksumMismatch {
                sentence: "file changed".into(),
            }
        );
        assert_eq!(
            classify_upload_commit(&answer(
                409,
                r#"{"error":"incomplete","reason_code":"incomplete_upload","detail":"short","next_offset":4194304}"#,
            )),
            UploadCommitReply::Incomplete {
                next_offset: Some(4_194_304),
            }
        );
        assert_eq!(
            classify_upload_commit(&answer(503, "")),
            UploadCommitReply::Retry
        );
    }

    #[test]
    fn retry_ladder_steps_through_contract_schedule_with_twenty_percent_jitter() {
        let mut ladder = RetryLadder::new();
        let expected_secs = [1, 2, 5, 10, 30, 60, 60];
        for secs in expected_secs {
            assert_eq!(
                ladder.next_delay_with_jitter(0.0),
                Duration::from_secs(secs)
            );
        }
        ladder.reset();
        assert_eq!(
            ladder.next_delay_with_jitter(-1.0),
            Duration::from_millis(800)
        );
        assert_eq!(
            ladder.next_delay_with_jitter(1.0),
            Duration::from_millis(2400)
        );
    }

    #[test]
    fn hash_file_and_read_chunk_stream_exact_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("blob.bin");
        let data: Vec<u8> = (0..(150 * 1024 + 13)).map(|i| (i % 251) as u8).collect();
        std::fs::write(&path, &data).unwrap();
        let (hex, len) = hash_file_sha256(&path).unwrap();
        assert_eq!(len, data.len() as u64);
        assert_eq!(hex, sha256_hex(&data));
        let slice = read_file_chunk(&path, 1024, 4096).unwrap();
        assert_eq!(slice, &data[1024..5120]);
    }
}
