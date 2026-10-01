// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Durable answer file for journal-mark confirmation.
//!
//! Stores the honest confirmation and rejection records for paired journals.
//! Never deleted by the app once written; uses atomic stage-sync-rename.

use std::io::{ErrorKind, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use crate::credential::{publish_staged_file, StorageError};
use crate::TransportError;

/// Derived answer file location beside the paired state path.
pub fn answer_path(state_path: &Path) -> PathBuf {
    state_path.with_file_name("pairing-answer.json")
}

/// Structured content of the pairing answer file.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AnswerState {
    pub confirmed: String,
    pub rejected: String,
}

fn is_valid_digest(s: &str) -> bool {
    s.is_empty()
        || (s.len() == 64
            && s.chars()
                .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c)))
}

/// Encode `AnswerState` into exact canonical bytes:
/// `{"confirmed":"<v>","rejected":"<v>"}\n`
pub fn encode_answer(state: &AnswerState) -> Result<Vec<u8>, StorageError> {
    if !is_valid_digest(&state.confirmed) || !is_valid_digest(&state.rejected) {
        return Err(StorageError::WriteFailed(std::io::Error::new(
            ErrorKind::InvalidInput,
            "invalid digest format in answer state",
        )));
    }
    let s = format!(
        "{{\"confirmed\":\"{}\",\"rejected\":\"{}\"}}\n",
        state.confirmed, state.rejected
    );
    Ok(s.into_bytes())
}

/// Parse canonical answer bytes. Rejects any deviation, extra whitespace, or invalid digest.
pub fn parse_answer(bytes: &[u8]) -> Result<AnswerState, StorageError> {
    let s = std::str::from_utf8(bytes)
        .map_err(|e| StorageError::WriteFailed(std::io::Error::new(ErrorKind::InvalidData, e)))?;
    let s = s.strip_suffix('\n').ok_or_else(|| {
        StorageError::WriteFailed(std::io::Error::new(
            ErrorKind::InvalidData,
            "missing trailing newline",
        ))
    })?;

    let prefix = "{\"confirmed\":\"";
    let middle = "\",\"rejected\":\"";
    let suffix = "\"}";

    if !s.starts_with(prefix) || !s.ends_with(suffix) {
        return Err(StorageError::WriteFailed(std::io::Error::new(
            ErrorKind::InvalidData,
            "malformed json shape",
        )));
    }

    let rest = &s[prefix.len()..s.len() - suffix.len()];
    let Some(split_pos) = rest.find(middle) else {
        return Err(StorageError::WriteFailed(std::io::Error::new(
            ErrorKind::InvalidData,
            "missing rejected field delimiter",
        )));
    };

    let confirmed = &rest[..split_pos];
    let rejected = &rest[split_pos + middle.len()..];

    if !is_valid_digest(confirmed) || !is_valid_digest(rejected) {
        return Err(StorageError::WriteFailed(std::io::Error::new(
            ErrorKind::InvalidData,
            "malformed digest field in answer",
        )));
    }

    Ok(AnswerState {
        confirmed: confirmed.to_string(),
        rejected: rejected.to_string(),
    })
}

/// Read the answer file if present. `None` when absent (`NotFound`).
/// Unreadable / malformed files return `Err(StorageError)` so bytes are preserved.
pub fn read_answer(path: &Path) -> Result<Option<AnswerState>, StorageError> {
    let mut file = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(StorageError::WriteFailed(e)),
    };
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)
        .map_err(StorageError::WriteFailed)?;
    let parsed = parse_answer(&bytes)?;
    Ok(Some(parsed))
}

/// Atomically write the answer file using stage-sync-rename.
pub fn write_answer(path: &Path, state: &AnswerState) -> Result<(), StorageError> {
    let bytes = encode_answer(state)?;
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(parent).map_err(StorageError::WriteFailed)?;
    let tmp = path.with_extension("json.tmp");
    let mut file = std::fs::File::create(&tmp).map_err(StorageError::WriteFailed)?;
    file.write_all(&bytes).map_err(StorageError::WriteFailed)?;
    file.sync_all().map_err(StorageError::WriteFailed)?;
    drop(file);

    publish_staged_file(&tmp, path)?;

    #[cfg(not(windows))]
    crate::credential::sync_published_path(path).map_err(StorageError::DurabilityUncertain)?;

    Ok(())
}

/// Settle grandfathering on launch or before a fresh ceremony.
///
/// If `pairing-answer.json` is absent (`NotFound`):
/// - If `pairing.json` is present and readable: writes `confirmed` = that PEM digest, `rejected` = empty.
/// - If `pairing.json` is absent: writes both empty.
/// - If `pairing.json` is unreadable: writes nothing.
///
/// If `pairing-answer.json` is already present: writes nothing.
pub fn settle_grandfather(
    state_path: &Path,
    cache: &Arc<Mutex<String>>,
) -> Result<(), StorageError> {
    let apath = answer_path(state_path);
    match read_answer(&apath) {
        Ok(Some(answer)) => {
            if let Ok(mut c) = cache.lock() {
                *c = answer.confirmed;
            }
            Ok(())
        }
        Ok(None) => {
            // Absent answer file. Inspect pairing.json
            match crate::credential::PairedState::load(state_path) {
                Ok(paired) => {
                    let (confirmed, rejected) = if let Some(cred) = paired.credential {
                        let digest =
                            crate::ack::JournalIdentity::from_credential(&cred).client_cert_sha256;
                        (digest, String::new())
                    } else {
                        (String::new(), String::new())
                    };
                    let state = AnswerState {
                        confirmed: confirmed.clone(),
                        rejected,
                    };
                    write_answer(&apath, &state)?;
                    if let Ok(mut c) = cache.lock() {
                        *c = confirmed;
                    }
                    Ok(())
                }
                Err(TransportError::Io(e)) if e.kind() == ErrorKind::NotFound => {
                    let state = AnswerState {
                        confirmed: String::new(),
                        rejected: String::new(),
                    };
                    write_answer(&apath, &state)?;
                    if let Ok(mut c) = cache.lock() {
                        c.clear();
                    }
                    Ok(())
                }
                Err(_) => {
                    // Unreadable pairing.json: write nothing, leave gate closed
                    Ok(())
                }
            }
        }
        Err(_) => {
            // Unreadable answer file: return Ok, do not write, do not clobber cache
            Ok(())
        }
    }
}

/// Settle previously recorded rejected pairings on launch.
///
/// If `rejected` matches the on-disk pairing's PEM digest:
/// Runs one `retire_client` against the journal (timeout 5s; 200/204/404 success).
/// Deletes `pairing.json` and `.json.tmp`. Clears `rejected` back to empty.
/// If `rejected` is set but does not match `pairing.json`, clears `rejected` only.
/// Returns `true` if launch resume should be skipped.
pub async fn settle_rejected_on_launch(
    state_path: &Path,
    confirmation: &Arc<Mutex<String>>,
    sync: &Arc<Mutex<observer_model::SyncSnapshot>>,
) -> bool {
    let apath = answer_path(state_path);
    let answer = match read_answer(&apath) {
        Ok(Some(a)) if !a.rejected.is_empty() => a,
        _ => return false,
    };

    let rejected_digest = answer.rejected.clone();
    match crate::credential::PairedState::load(state_path) {
        Ok(paired) => {
            let Some(cred) = paired.credential else {
                let mut updated = answer;
                updated.rejected.clear();
                let _ = write_answer(&apath, &updated);
                return false;
            };

            let cert_digest =
                crate::ack::JournalIdentity::from_credential(&cred).client_cert_sha256;
            if cert_digest == rejected_digest {
                // Matching rejected pairing found. Attempt one retire_client
                if let Ok(der_certs) = spl_transport::tls::parse_certs(&cred.client_cert_pem) {
                    if let Some(der) = der_certs.first() {
                        let client_id =
                            format!("sha256:{}", spl_core::ca::sha256_hex(der.as_ref()));
                        if let Ok(client) = crate::client::ObserverClient::new(
                            cred.clone(),
                            Arc::new(std::sync::atomic::AtomicBool::new(false)),
                        ) {
                            let _ = tokio::time::timeout(
                                std::time::Duration::from_secs(5),
                                client.retire_client(&client_id),
                            )
                            .await;
                        }
                    }
                }

                // Delete pairing.json and pairing.json.tmp
                let tmp = state_path.with_extension("json.tmp");
                let mut delete_ok = true;
                if state_path.exists() && std::fs::remove_file(state_path).is_err() {
                    delete_ok = false;
                }
                if tmp.exists() {
                    let _ = std::fs::remove_file(&tmp);
                }

                if delete_ok {
                    let mut updated = answer;
                    updated.rejected.clear();
                    let _ = write_answer(&apath, &updated);
                    crate::service::publish_pairing(
                        sync,
                        confirmation,
                        crate::service::PairingWrite::NotPaired { detail: None },
                    );
                    false
                } else {
                    true
                }
            } else {
                let mut updated = answer;
                updated.rejected.clear();
                let _ = write_answer(&apath, &updated);
                false
            }
        }
        Err(TransportError::Io(e)) if e.kind() == ErrorKind::NotFound => {
            let mut updated = answer;
            updated.rejected.clear();
            let _ = write_answer(&apath, &updated);
            false
        }
        Err(_) => {
            // Unreadable pairing.json: do not clear rejected, skip resume
            true
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TestDir(PathBuf);
    impl TestDir {
        fn new(name: &str) -> Self {
            let p = PathBuf::from("/var/tmp").join(format!(
                "pl-transport-{}-{}",
                name,
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            let _ = std::fs::remove_dir_all(&p);
            std::fs::create_dir_all(&p).unwrap();
            Self(p)
        }
        fn path(&self) -> &Path {
            &self.0
        }
    }
    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn mark_confirmation_codec_round_trip() {
        let empty = AnswerState::default();
        let bytes = encode_answer(&empty).expect("encode empty");
        assert_eq!(bytes, b"{\"confirmed\":\"\",\"rejected\":\"\"}\n");
        let parsed = parse_answer(&bytes).expect("parse empty");
        assert_eq!(parsed, empty);

        let hex64_1 = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        let hex64_2 = "fedcba9876543210fedcba9876543210fedcba9876543210fedcba9876543210";
        let populated = AnswerState {
            confirmed: hex64_1.to_string(),
            rejected: hex64_2.to_string(),
        };
        let bytes = encode_answer(&populated).expect("encode populated");
        assert_eq!(
            bytes,
            format!(
                "{{\"confirmed\":\"{}\",\"rejected\":\"{}\"}}\n",
                hex64_1, hex64_2
            )
            .into_bytes()
        );
        let parsed = parse_answer(&bytes).expect("parse populated");
        assert_eq!(parsed, populated);
    }

    #[test]
    fn mark_confirmation_codec_rejections() {
        // Uppercase hex
        let invalid = AnswerState {
            confirmed: "A".repeat(64),
            rejected: String::new(),
        };
        assert!(encode_answer(&invalid).is_err());

        // Short length
        let invalid = AnswerState {
            confirmed: "a".repeat(63),
            rejected: String::new(),
        };
        assert!(encode_answer(&invalid).is_err());

        // Long length
        let invalid = AnswerState {
            confirmed: "a".repeat(65),
            rejected: String::new(),
        };
        assert!(encode_answer(&invalid).is_err());

        // Invalid chars
        let invalid = AnswerState {
            confirmed: "g".repeat(64),
            rejected: String::new(),
        };
        assert!(encode_answer(&invalid).is_err());

        // Missing newline
        assert!(parse_answer(b"{\"confirmed\":\"\",\"rejected\":\"\"}").is_err());

        // Extra whitespace
        assert!(parse_answer(b"{\"confirmed\":\"\", \"rejected\":\"\"}\n").is_err());
        assert!(parse_answer(b" {\"confirmed\":\"\",\"rejected\":\"\"}\n").is_err());
        assert!(parse_answer(b"{\"confirmed\":\"\",\"rejected\":\"\"} \n").is_err());

        // Extra fields
        assert!(parse_answer(b"{\"confirmed\":\"\",\"rejected\":\"\",\"other\":\"\"}\n").is_err());

        // Missing field
        assert!(parse_answer(b"{\"confirmed\":\"\"}\n").is_err());
    }

    #[test]
    fn mark_confirmation_read_write_atomic() {
        let dir = TestDir::new("atomic");
        let spath = dir.path().join("pairing.json");
        let apath = answer_path(&spath);

        assert_eq!(read_answer(&apath).unwrap(), None);

        let hex64 = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        let state = AnswerState {
            confirmed: hex64.to_string(),
            rejected: String::new(),
        };
        write_answer(&apath, &state).unwrap();
        assert_eq!(read_answer(&apath).unwrap(), Some(state));

        // Corrupted file errors
        std::fs::write(&apath, b"corrupted file\n").unwrap();
        assert!(read_answer(&apath).is_err());
    }

    #[test]
    fn mark_confirmation_settle_grandfather_absent_pairing() {
        let dir = TestDir::new("grandfather");
        let spath = dir.path().join("pairing.json");
        let cache = Arc::new(Mutex::new("initial".to_string()));

        settle_grandfather(&spath, &cache).unwrap();
        let apath = answer_path(&spath);
        let ans = read_answer(&apath).unwrap().expect("answer file written");
        assert_eq!(ans.confirmed, "");
        assert_eq!(ans.rejected, "");
        assert_eq!(*cache.lock().unwrap(), "");
    }
}
