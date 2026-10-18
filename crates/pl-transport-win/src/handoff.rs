// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Setup handoff receiver mailbox transaction and progress management.
//!
//! Owns the exclusive file-locked transaction for processing the sender mailbox,
//! persisting progress and a resume receipt, and conditionally consuming the mailbox.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use crate::credential::{PairedState, PAIRED_STATE_FILE_NAME};
use observer_contract::{
    is_canonical_uuid, HANDOFF_REQUIRED_STEP, LOCK_ACQUIRE_MS, LOCK_FILE_NAME, MAILBOX_FILE_NAME,
    MAILBOX_MAX_BYTES, MAILBOX_SCHEMA, PROGRESS_FILE_NAME, PROGRESS_MAX_BYTES, PROGRESS_SCHEMA,
    RECEIPT_FILE_NAME, RECEIPT_MAX_BYTES, RECEIPT_SCHEMA, UNPAIRED_DRAFT_MAX_AGE_SECS,
};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SetupHandoffMailbox {
    pub schema: String,
    pub handoff_id: String,
    pub step: u32,
    pub expected_instance_id: String,
    pub expected_ca_fingerprint: String,
    pub pairing_link: String,
}

impl SetupHandoffMailbox {
    pub fn validate(&self) -> bool {
        if self.schema != MAILBOX_SCHEMA || !is_canonical_uuid(&self.handoff_id) {
            return false;
        }
        if self.step != HANDOFF_REQUIRED_STEP {
            return false;
        }
        valid_expectations(
            &self.expected_ca_fingerprint,
            &self.expected_instance_id,
            &self.pairing_link,
        )
    }
}

fn valid_expectations(fingerprint: &str, instance_id: &str, link: &str) -> bool {
    fingerprint.len() == 64
        && fingerprint
            .bytes()
            .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
        && !instance_id.is_empty()
        && instance_id.len() <= 128
        && !instance_id.chars().any(char::is_whitespace)
        && canonical_https_pair_link(link)
}

fn canonical_https_pair_link(link: &str) -> bool {
    let Some(fragment) = link.strip_prefix(observer_contract::CANONICAL_PREFIX) else {
        return false;
    };
    if fragment.is_empty()
        || fragment.contains('#')
        || fragment.contains('/')
        || fragment.contains('?')
        || fragment.chars().any(char::is_whitespace)
    {
        return false;
    }
    spl_core::pairlink::parse(link).is_ok()
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SetupHandoffProgress {
    pub schema: String,
    pub handoff_id: String,
    pub pairing_link: String,
    pub expected_ca_fingerprint: String,
    pub expected_instance_id: String,
    pub accepted_unix_ms: u64,
}

impl SetupHandoffProgress {
    pub fn validate(&self) -> bool {
        self.schema == PROGRESS_SCHEMA
            && is_canonical_uuid(&self.handoff_id)
            && valid_expectations(
                &self.expected_ca_fingerprint,
                &self.expected_instance_id,
                &self.pairing_link,
            )
    }

    pub fn is_expired(&self, now_unix_ms: u64) -> bool {
        now_unix_ms.saturating_sub(self.accepted_unix_ms) >= UNPAIRED_DRAFT_MAX_AGE_SECS * 1000
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SetupHandoffReceipt {
    pub schema: String,
    pub handoff_id: String,
    pub accepted: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MailboxOutcome {
    Absent,
    Accepted,
    Replay,
    Deferred,
    Rejected,
    Busy,
}

pub enum HandoffLock {
    Held(File),
    Busy,
}

pub struct PairingAdmission<'a> {
    active: &'a AtomicBool,
}

impl Drop for PairingAdmission<'_> {
    fn drop(&mut self) {
        self.active.store(false, Ordering::Release);
    }
}

/// Claim pairing independently of status publications, while validating the
/// displayed continuation under the same lock used by mailbox admission.
pub fn claim_pairing_admission<'a, F, T>(
    data_root: &Path,
    active: &'a AtomicBool,
    validate: F,
) -> Result<(PairingAdmission<'a>, T), crate::TransportError>
where
    F: FnOnce() -> Result<T, crate::TransportError>,
{
    let lock = match acquire_handoff_lock(data_root)? {
        HandoffLock::Held(file) => file,
        HandoffLock::Busy => {
            return Err(crate::TransportError::Pairing(
                "setup pairing is busy".into(),
            ))
        }
    };
    active
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .map_err(|_| crate::TransportError::Pairing("setup pairing is busy".into()))?;
    let guard = PairingAdmission { active };
    let result = validate()?;
    drop(lock);
    Ok((guard, result))
}

/// Create the data root, then take the handoff lock without truncating or removing it.
pub fn acquire_handoff_lock(data_root: &Path) -> io::Result<HandoffLock> {
    fs::create_dir_all(data_root)?;
    let lock_file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(data_root.join(LOCK_FILE_NAME))?;
    let start = Instant::now();
    let timeout = Duration::from_millis(LOCK_ACQUIRE_MS);
    loop {
        match lock_file.try_lock() {
            Ok(()) => return Ok(HandoffLock::Held(lock_file)),
            Err(std::fs::TryLockError::WouldBlock) => {
                if start.elapsed() >= timeout {
                    return Ok(HandoffLock::Busy);
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(std::fs::TryLockError::Error(error)) => return Err(error),
        }
    }
}

/// Read at most `limit` bytes. One extra byte detects an oversize file without
/// loading an unbounded document.
pub fn read_bounded(path: &Path, limit: usize) -> io::Result<Vec<u8>> {
    let file = File::open(path)?;
    let mut limited = file.take(u64::try_from(limit).unwrap_or(u64::MAX).saturating_add(1));
    let mut buf = Vec::new();
    limited.read_to_end(&mut buf)?;
    if buf.len() > limit {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "document exceeds maximum bytes",
        ));
    }
    Ok(buf)
}

fn write_atomic_json<T: Serialize>(path: &Path, value: &T, limit: usize) -> io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::other("missing parent directory"))?;
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("temp_doc");
    let tmp = parent.join(format!(
        "{file_name}.{}.{}.tmp",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    ));
    let bytes = serde_json::to_vec_pretty(value)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    if bytes.len() > limit {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "document exceeds maximum bytes",
        ));
    }
    let write_result = (|| {
        let mut file = File::create(&tmp)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        drop(file);
        crate::credential::publish_staged_file(&tmp, path)
            .map_err(|error| io::Error::other(error.to_string()))?;
        #[cfg(not(windows))]
        crate::credential::sync_published_path(path)?;
        Ok(())
    })();
    if write_result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    write_result
}

pub fn read_progress(data_root: &Path) -> io::Result<Option<SetupHandoffProgress>> {
    let path = data_root.join(PROGRESS_FILE_NAME);
    let bytes = match read_bounded(&path, PROGRESS_MAX_BYTES) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let progress: SetupHandoffProgress = serde_json::from_slice(&bytes)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    if !progress.validate() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "progress rejected",
        ));
    }
    Ok(Some(progress))
}

pub fn read_active_progress(data_root: &Path) -> io::Result<Option<SetupHandoffProgress>> {
    let progress = read_progress(data_root)?;
    let retired = PairedState::setup_handoff_history(&data_root.join(PAIRED_STATE_FILE_NAME))
        .map_err(|error| io::Error::other(error.to_string()))?;
    Ok(progress.filter(|progress| !retired.contains(&progress.handoff_id)))
}

fn read_receipt_doc(data_root: &Path) -> Option<SetupHandoffReceipt> {
    let bytes = read_bounded(&data_root.join(RECEIPT_FILE_NAME), RECEIPT_MAX_BYTES).ok()?;
    serde_json::from_slice(&bytes).ok()
}

pub fn read_mailbox_doc(data_root: &Path) -> io::Result<Option<SetupHandoffMailbox>> {
    let path = data_root.join(MAILBOX_FILE_NAME);
    let bytes = match read_bounded(&path, MAILBOX_MAX_BYTES) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let mailbox: SetupHandoffMailbox = serde_json::from_slice(&bytes)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    if mailbox.validate() {
        Ok(Some(mailbox))
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "mailbox rejected",
        ))
    }
}

pub fn process_mailbox(
    data_root: &Path,
    now_unix_ms: u64,
    ceremony_active: bool,
) -> io::Result<MailboxOutcome> {
    process_mailbox_with_hooks(
        data_root,
        now_unix_ms,
        || ceremony_active,
        |_, _| Ok(()),
        |_, _| Ok(()),
    )
}

pub fn process_mailbox_live<F>(
    data_root: &Path,
    now_unix_ms: u64,
    ceremony_active: F,
) -> io::Result<MailboxOutcome>
where
    F: FnOnce() -> bool,
{
    process_mailbox_with_hooks(
        data_root,
        now_unix_ms,
        ceremony_active,
        |_, _| Ok(()),
        |_, _| Ok(()),
    )
}

pub fn process_mailbox_with_hooks<FProgress, FReceipt>(
    data_root: &Path,
    now_unix_ms: u64,
    ceremony_active: impl FnOnce() -> bool,
    before_progress: FProgress,
    after_receipt: FReceipt,
) -> io::Result<MailboxOutcome>
where
    FProgress: FnOnce(&Path, &SetupHandoffProgress) -> io::Result<()>,
    FReceipt: FnOnce(&Path, &SetupHandoffReceipt) -> io::Result<()>,
{
    let mailbox_path = data_root.join(MAILBOX_FILE_NAME);
    if !mailbox_path.exists() {
        return Ok(MailboxOutcome::Absent);
    }

    let _lock = match acquire_handoff_lock(data_root)? {
        HandoffLock::Held(file) => file,
        HandoffLock::Busy => return Ok(MailboxOutcome::Busy),
    };

    let mailbox = match read_mailbox_doc(data_root) {
        Ok(Some(mailbox)) => mailbox,
        Ok(None) => return Ok(MailboxOutcome::Absent),
        Err(_) => return Ok(MailboxOutcome::Rejected),
    };

    let state_path = data_root.join(PAIRED_STATE_FILE_NAME);
    let retired = match PairedState::setup_handoff_history(&state_path) {
        Ok(retired) => retired,
        Err(error) => {
            invalidate_matching_receipt(data_root, &mailbox.handoff_id)?;
            return Err(io::Error::other(error.to_string()));
        }
    };
    if retired.contains(&mailbox.handoff_id) {
        repair_receipt(data_root, &mailbox.handoff_id)?;
        consume_mailbox_if_same(data_root, &mailbox_path, &mailbox.handoff_id);
        return Ok(MailboxOutcome::Replay);
    }
    let existing_progress = match read_progress(data_root) {
        Ok(progress) => progress,
        Err(error) => {
            invalidate_matching_receipt(data_root, &mailbox.handoff_id)?;
            return Err(error);
        }
    };

    if let Some(progress) = existing_progress.as_ref() {
        if progress.handoff_id == mailbox.handoff_id {
            repair_receipt(data_root, &mailbox.handoff_id)?;
            consume_mailbox_if_same(data_root, &mailbox_path, &mailbox.handoff_id);
            return Ok(MailboxOutcome::Replay);
        }
    }

    invalidate_matching_receipt(data_root, &mailbox.handoff_id)?;
    if ceremony_active() {
        return Ok(MailboxOutcome::Deferred);
    }
    if let Some(progress) = existing_progress.as_ref() {
        if !retired.contains(&progress.handoff_id) && !progress.is_expired(now_unix_ms) {
            return Ok(MailboxOutcome::Deferred);
        }
    }

    PairedState::check_setup_handoff_capacity(
        &state_path,
        existing_progress
            .as_ref()
            .map(|progress| progress.handoff_id.as_str()),
        &mailbox.handoff_id,
    )
    .map_err(|error| io::Error::other(error.to_string()))?;
    if let Some(progress) = existing_progress.as_ref() {
        if !retired.contains(&progress.handoff_id) {
            PairedState::retire_setup_handoff(&state_path, &progress.handoff_id)
                .map_err(|error| io::Error::other(error.to_string()))?;
        }
    }

    let progress = SetupHandoffProgress {
        schema: PROGRESS_SCHEMA.to_string(),
        handoff_id: mailbox.handoff_id.clone(),
        pairing_link: mailbox.pairing_link.clone(),
        expected_ca_fingerprint: mailbox.expected_ca_fingerprint.clone(),
        expected_instance_id: mailbox.expected_instance_id.clone(),
        accepted_unix_ms: now_unix_ms,
    };
    before_progress(data_root, &progress)?;
    write_atomic_json(
        &data_root.join(PROGRESS_FILE_NAME),
        &progress,
        PROGRESS_MAX_BYTES,
    )?;

    let receipt = SetupHandoffReceipt {
        schema: RECEIPT_SCHEMA.to_string(),
        handoff_id: mailbox.handoff_id.clone(),
        accepted: true,
    };
    write_atomic_json(
        &data_root.join(RECEIPT_FILE_NAME),
        &receipt,
        RECEIPT_MAX_BYTES,
    )?;
    after_receipt(data_root, &receipt)?;
    consume_mailbox_if_same(data_root, &mailbox_path, &mailbox.handoff_id);
    Ok(MailboxOutcome::Accepted)
}

fn invalidate_matching_receipt(data_root: &Path, handoff_id: &str) -> io::Result<()> {
    if read_receipt_doc(data_root).is_some_and(|receipt| {
        receipt.schema == RECEIPT_SCHEMA && receipt.handoff_id == handoff_id && receipt.accepted
    }) {
        fs::remove_file(data_root.join(RECEIPT_FILE_NAME))?;
    }
    Ok(())
}

fn repair_receipt(data_root: &Path, handoff_id: &str) -> io::Result<()> {
    let needs_receipt = match read_receipt_doc(data_root) {
        Some(receipt) => {
            receipt.schema != RECEIPT_SCHEMA
                || receipt.handoff_id != handoff_id
                || !receipt.accepted
        }
        None => true,
    };
    if needs_receipt {
        let receipt = SetupHandoffReceipt {
            schema: RECEIPT_SCHEMA.to_string(),
            handoff_id: handoff_id.to_string(),
            accepted: true,
        };
        write_atomic_json(
            &data_root.join(RECEIPT_FILE_NAME),
            &receipt,
            RECEIPT_MAX_BYTES,
        )?;
    }
    Ok(())
}

fn consume_mailbox_if_same(data_root: &Path, mailbox_path: &Path, handoff_id: &str) {
    if let Ok(Some(current)) = read_mailbox_doc(data_root) {
        if current.handoff_id == handoff_id {
            let _ = fs::remove_file(mailbox_path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    const SAMPLE_LINK: &str = "https://go.solstone.app/p#0G0W000258DSX8DJRFAEBXG7308J4CT4ANK7F26YNPZEZJQYQAZ028T5CY4TQKFF";
    const SAMPLE_FP: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
    const ID_A: &str = "11111111-1111-4111-8111-111111111111";
    const ID_B: &str = "22222222-2222-4222-8222-222222222222";

    struct TestDir(PathBuf);
    impl TestDir {
        fn new(prefix: &str) -> Self {
            let base = if Path::new("/var/tmp").exists() {
                PathBuf::from("/var/tmp")
            } else {
                std::env::temp_dir()
            };
            let path = base.join(format!(
                "handoff-test-{prefix}-{}-{}",
                std::process::id(),
                observer_model::monotonic_us()
            ));
            let _ = fs::create_dir_all(&path);
            Self(path)
        }
        fn path(&self) -> &Path {
            &self.0
        }
    }
    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn sample_mailbox(id: &str) -> SetupHandoffMailbox {
        SetupHandoffMailbox {
            schema: MAILBOX_SCHEMA.to_string(),
            handoff_id: id.to_string(),
            step: HANDOFF_REQUIRED_STEP,
            pairing_link: SAMPLE_LINK.to_string(),
            expected_ca_fingerprint: SAMPLE_FP.to_string(),
            expected_instance_id: "test-instance-id".to_string(),
        }
    }

    fn write_mailbox(dir: &Path, mailbox: &SetupHandoffMailbox) {
        write_atomic_json(&dir.join(MAILBOX_FILE_NAME), mailbox, MAILBOX_MAX_BYTES).unwrap();
    }

    fn restore_dir_writable(path: &Path) {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = fs::metadata(path).unwrap().permissions();
            perms.set_mode(0o700);
            fs::set_permissions(path, perms).unwrap();
        }
        #[cfg(not(unix))]
        {
            let mut perms = fs::metadata(path).unwrap().permissions();
            #[allow(clippy::permissions_set_readonly_false)]
            perms.set_readonly(false);
            fs::set_permissions(path, perms).unwrap();
        }
    }

    #[test]
    fn literal_mailbox_document_validates_and_legacy_document_does_not() {
        let literal = r#"{
            "schema": "solstone.windows-setup-handoff.v1",
            "handoff_id": "11111111-1111-4111-8111-111111111111",
            "step": 5,
            "expected_instance_id": "test-instance-id",
            "expected_ca_fingerprint": "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
            "pairing_link": "https://go.solstone.app/p#0G0W000258DSX8DJRFAEBXG7308J4CT4ANK7F26YNPZEZJQYQAZ028T5CY4TQKFF"
        }"#;
        let mailbox: SetupHandoffMailbox = serde_json::from_str(literal).unwrap();
        assert!(mailbox.validate());

        let legacy = r#"{
            "schema": "solstone.setup-handoff.v1",
            "id": "11111111-1111-4111-8111-111111111111",
            "step": 5,
            "link": "https://go.solstone.app/p#0G0W000258DSX8DJRFAEBXG7308J4CT4ANK7F26YNPZEZJQYQAZ028T5CY4TQKFF",
            "expected_ca_fingerprint": "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
            "expected_instance_id": "test-instance-id"
        }"#;
        assert!(serde_json::from_str::<SetupHandoffMailbox>(legacy).is_err());

        let dir = TestDir::new("legacy-name");
        let legacy_path = dir.path().join("setup-handoff.json");
        fs::write(&legacy_path, legacy.as_bytes()).unwrap();
        let before = fs::read(&legacy_path).unwrap();
        let outcome = process_mailbox(dir.path(), 1000, false).unwrap();
        assert_eq!(outcome, MailboxOutcome::Absent);
        assert_eq!(fs::read(&legacy_path).unwrap(), before);
        assert!(!dir.path().join(RECEIPT_FILE_NAME).exists());
    }

    #[test]
    fn mailbox_validation_reject_table() {
        let valid = sample_mailbox(ID_A);
        assert!(valid.validate());

        let mut bad_schema = valid.clone();
        bad_schema.schema = "solstone.setup-handoff.v1".to_string();
        assert!(!bad_schema.validate());

        let mut bad_step = valid.clone();
        bad_step.step = 4;
        assert!(!bad_step.validate());

        let mut uppercase = valid.clone();
        uppercase.handoff_id = "11111111-1111-4111-8111-11111111111A".to_string();
        assert!(!uppercase.validate());

        let mut bare = valid.clone();
        bare.handoff_id = "11111111111141118111111111111111".to_string();
        assert!(!bare.validate());

        let mut desktop = valid.clone();
        desktop.pairing_link =
            "solstone://pair#0G0W000258DSX8DJRFAEBXG7308J4CT4ANK7F26YNPZEZJQYQAZ028T5CY4TQKFF"
                .to_string();
        assert!(!desktop.validate());

        let mut bad_link = valid.clone();
        bad_link.pairing_link = "https://example.invalid/not-a-pair-link".to_string();
        assert!(!bad_link.validate());

        let mut bad_fp = valid.clone();
        bad_fp.expected_ca_fingerprint = "0123456789abcdef".to_string();
        assert!(!bad_fp.validate());

        let mut upper_fp = valid.clone();
        upper_fp.expected_ca_fingerprint = SAMPLE_FP.to_uppercase();
        assert!(!upper_fp.validate());
    }

    #[test]
    fn unknown_or_missing_fields_are_rejected_and_kept() {
        let dir = TestDir::new("unknown");
        let path = dir.path().join(MAILBOX_FILE_NAME);
        fs::write(
            &path,
            br#"{"schema":"solstone.windows-setup-handoff.v1","handoff_id":"11111111-1111-4111-8111-111111111111","step":5,"extra":true}"#,
        )
        .unwrap();
        assert_eq!(
            process_mailbox(dir.path(), 1000, false).unwrap(),
            MailboxOutcome::Rejected
        );
        assert!(path.exists());
        assert!(!dir.path().join(RECEIPT_FILE_NAME).exists());

        fs::write(
            &path,
            br#"{"schema":"solstone.windows-setup-handoff.v1","handoff_id":"11111111-1111-4111-8111-111111111111","step":5}"#,
        )
        .unwrap();
        assert_eq!(
            process_mailbox(dir.path(), 1000, false).unwrap(),
            MailboxOutcome::Rejected
        );
        assert!(!dir.path().join(RECEIPT_FILE_NAME).exists());
    }

    #[test]
    fn bounded_read_rejects_limit_plus_one_from_memory() {
        let dir = TestDir::new("limit");
        let path = dir.path().join("mem.json");
        let bytes = vec![b'x'; MAILBOX_MAX_BYTES + 1];
        fs::write(&path, &bytes).unwrap();
        let error = read_bounded(&path, MAILBOX_MAX_BYTES).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn mailbox_accept_and_cleanup() {
        let dir = TestDir::new("accept");
        let mailbox = sample_mailbox(ID_A);
        write_mailbox(dir.path(), &mailbox);

        assert_eq!(
            process_mailbox(dir.path(), 1000, false).unwrap(),
            MailboxOutcome::Accepted
        );
        assert!(!dir.path().join(MAILBOX_FILE_NAME).exists());
        let progress = read_progress(dir.path()).unwrap().unwrap();
        assert_eq!(progress.handoff_id, ID_A);
        assert_eq!(progress.accepted_unix_ms, 1000);
        assert_eq!(progress.pairing_link, SAMPLE_LINK);
        let receipt = read_receipt_doc(dir.path()).unwrap();
        assert_eq!(receipt.handoff_id, ID_A);
        assert!(receipt.accepted);
    }

    #[test]
    fn corrupt_progress_is_an_error_and_does_not_accept() {
        let dir = TestDir::new("corrupt-progress");
        write_mailbox(dir.path(), &sample_mailbox(ID_A));
        let progress_path = dir.path().join(PROGRESS_FILE_NAME);
        fs::write(&progress_path, b"{not json").unwrap();
        let before = fs::read(&progress_path).unwrap();
        assert!(process_mailbox(dir.path(), 1000, false).is_err());
        assert!(dir.path().join(MAILBOX_FILE_NAME).exists());
        assert!(!dir.path().join(RECEIPT_FILE_NAME).exists());
        assert_eq!(fs::read(&progress_path).unwrap(), before);
    }

    #[test]
    fn replay_keeps_accepted_unix_ms_and_repairs_the_receipt() {
        let dir = TestDir::new("replay");
        let mailbox = sample_mailbox(ID_A);
        write_mailbox(dir.path(), &mailbox);
        assert_eq!(
            process_mailbox(dir.path(), 1000, false).unwrap(),
            MailboxOutcome::Accepted
        );

        write_mailbox(dir.path(), &mailbox);
        fs::remove_file(dir.path().join(RECEIPT_FILE_NAME)).unwrap();
        assert_eq!(
            process_mailbox(dir.path(), 2000, false).unwrap(),
            MailboxOutcome::Replay
        );
        let progress = read_progress(dir.path()).unwrap().unwrap();
        assert_eq!(progress.accepted_unix_ms, 1000);
        assert!(read_receipt_doc(dir.path()).unwrap().accepted);
        assert!(!dir.path().join(MAILBOX_FILE_NAME).exists());
    }

    #[test]
    fn progress_write_failure_leaves_the_mailbox_and_skips_the_receipt() {
        let dir = TestDir::new("progress-fail");
        write_mailbox(dir.path(), &sample_mailbox(ID_A));
        let error = process_mailbox_with_hooks(
            dir.path(),
            1000,
            || false,
            |_, _| Err(io::Error::other("progress hook")),
            |_, _| Ok(()),
        );
        assert!(error.is_err());
        assert!(dir.path().join(MAILBOX_FILE_NAME).exists());
        assert!(!dir.path().join(RECEIPT_FILE_NAME).exists());
    }

    #[test]
    fn receipt_failure_leaves_mailbox() {
        let dir = TestDir::new("receipt-fail");
        write_mailbox(dir.path(), &sample_mailbox(ID_A));
        let error = process_mailbox_with_hooks(
            dir.path(),
            1000,
            || false,
            |_, _| Ok(()),
            |_, _| Err(io::Error::other("receipt hook error")),
        );
        assert!(error.is_err());
        assert!(dir.path().join(MAILBOX_FILE_NAME).exists());
    }

    #[test]
    fn consume_failure_still_accepts() {
        let dir = TestDir::new("consume-fail");
        write_mailbox(dir.path(), &sample_mailbox(ID_A));
        let outcome = process_mailbox_with_hooks(
            dir.path(),
            1000,
            || false,
            |_, _| Ok(()),
            |data_root, _| {
                let mut perms = fs::metadata(data_root).unwrap().permissions();
                perms.set_readonly(true);
                fs::set_permissions(data_root, perms).unwrap();
                Ok(())
            },
        )
        .unwrap();
        restore_dir_writable(dir.path());
        assert_eq!(outcome, MailboxOutcome::Accepted);
        assert!(dir.path().join(MAILBOX_FILE_NAME).exists());
        assert!(read_progress(dir.path()).unwrap().is_some());
        assert!(read_receipt_doc(dir.path()).unwrap().accepted);
    }

    #[test]
    fn newer_unexpired_id_is_deferred_without_a_receipt() {
        let dir = TestDir::new("defer-newer");
        write_mailbox(dir.path(), &sample_mailbox(ID_A));
        assert_eq!(
            process_mailbox(dir.path(), 1_000, false).unwrap(),
            MailboxOutcome::Accepted
        );
        fs::remove_file(dir.path().join(RECEIPT_FILE_NAME)).unwrap();
        write_mailbox(dir.path(), &sample_mailbox(ID_B));
        assert_eq!(
            process_mailbox(dir.path(), 2_000, false).unwrap(),
            MailboxOutcome::Deferred
        );
        assert!(dir.path().join(MAILBOX_FILE_NAME).exists());
        assert!(!dir.path().join(RECEIPT_FILE_NAME).exists());
        let progress = read_progress(dir.path()).unwrap().unwrap();
        assert_eq!(progress.handoff_id, ID_A);
        assert_eq!(progress.accepted_unix_ms, 1_000);
    }

    #[test]
    fn ceremony_defers_without_a_receipt_or_progress_change() {
        let dir = TestDir::new("defer-ceremony");
        write_mailbox(dir.path(), &sample_mailbox(ID_A));
        assert_eq!(
            process_mailbox(dir.path(), 1_000, false).unwrap(),
            MailboxOutcome::Accepted
        );
        fs::remove_file(dir.path().join(RECEIPT_FILE_NAME)).unwrap();
        write_mailbox(dir.path(), &sample_mailbox(ID_B));
        assert_eq!(
            process_mailbox(dir.path(), 2_000_000, true).unwrap(),
            MailboxOutcome::Deferred
        );
        assert!(!dir.path().join(RECEIPT_FILE_NAME).exists());
        let progress = read_progress(dir.path()).unwrap().unwrap();
        assert_eq!(progress.handoff_id, ID_A);
        assert_eq!(progress.accepted_unix_ms, 1_000);
    }

    #[test]
    fn progress_expiry_logic() {
        let progress = SetupHandoffProgress {
            schema: PROGRESS_SCHEMA.to_string(),
            handoff_id: ID_A.to_string(),
            pairing_link: SAMPLE_LINK.to_string(),
            expected_ca_fingerprint: SAMPLE_FP.to_string(),
            expected_instance_id: "inst-1".to_string(),
            accepted_unix_ms: 10_000,
        };
        assert!(!progress.is_expired(10_000 + 899 * 1000));
        assert!(progress.is_expired(10_000 + 900 * 1000));
        assert!(!progress.is_expired(5_000));
    }

    #[test]
    fn retired_h1_never_reopens_and_fresh_h2_is_immediately_usable() {
        let dir = TestDir::new("retired-replay");
        let state_path = dir.path().join(PAIRED_STATE_FILE_NAME);
        write_mailbox(dir.path(), &sample_mailbox(ID_A));
        assert_eq!(
            process_mailbox(dir.path(), 1_000, false).unwrap(),
            MailboxOutcome::Accepted
        );
        PairedState::retire_setup_handoff(&state_path, ID_A).unwrap();
        assert!(read_active_progress(dir.path()).unwrap().is_none());

        write_mailbox(dir.path(), &sample_mailbox(ID_A));
        assert_eq!(
            process_mailbox(dir.path(), 2_000, false).unwrap(),
            MailboxOutcome::Replay
        );
        assert!(read_active_progress(dir.path()).unwrap().is_none());
        write_mailbox(dir.path(), &sample_mailbox(ID_B));
        assert_eq!(
            process_mailbox(dir.path(), 3_000, false).unwrap(),
            MailboxOutcome::Accepted
        );
        let h2 = read_active_progress(dir.path()).unwrap().unwrap();
        assert_eq!(h2.handoff_id, ID_B);

        write_mailbox(dir.path(), &sample_mailbox(ID_A));
        assert_eq!(
            process_mailbox(dir.path(), 3_000_000, false).unwrap(),
            MailboxOutcome::Replay
        );
        assert_eq!(read_active_progress(dir.path()).unwrap(), Some(h2));
    }

    #[test]
    fn expired_replacement_retires_the_old_id_before_new_acceptance() {
        let dir = TestDir::new("expired-replacement");
        write_mailbox(dir.path(), &sample_mailbox(ID_A));
        process_mailbox(dir.path(), 1_000, false).unwrap();
        write_mailbox(dir.path(), &sample_mailbox(ID_B));
        assert_eq!(
            process_mailbox(dir.path(), 2_000_000, false).unwrap(),
            MailboxOutcome::Accepted
        );
        write_mailbox(dir.path(), &sample_mailbox(ID_A));
        assert_eq!(
            process_mailbox(dir.path(), 4_000_000, false).unwrap(),
            MailboxOutcome::Replay
        );
        assert_eq!(read_progress(dir.path()).unwrap().unwrap().handoff_id, ID_B);
    }

    #[test]
    fn orphan_receipt_is_removed_before_a_real_progress_publication_failure() {
        let dir = TestDir::new("orphan-real-write-failure");
        write_mailbox(dir.path(), &sample_mailbox(ID_A));
        repair_receipt(dir.path(), ID_A).unwrap();
        assert!(read_receipt_doc(dir.path()).unwrap().accepted);
        assert!(read_progress(dir.path()).unwrap().is_none());
        let result = process_mailbox_with_hooks(
            dir.path(),
            1_000,
            || false,
            |root, _| {
                assert!(!root.join(RECEIPT_FILE_NAME).exists());
                fs::create_dir(root.join(PROGRESS_FILE_NAME))?;
                Ok(())
            },
            |_, _| panic!("receipt cannot publish after progress publication failed"),
        );
        assert!(result.is_err());
        assert!(dir.path().join(MAILBOX_FILE_NAME).exists());
        assert!(!dir.path().join(RECEIPT_FILE_NAME).exists());
        assert!(read_progress(dir.path()).is_err());
    }

    #[test]
    fn corrupt_persisted_progress_is_refused_before_native_authority() {
        let dir = TestDir::new("corrupt-progress");
        let mailbox = sample_mailbox(ID_A);
        let progress = SetupHandoffProgress {
            schema: PROGRESS_SCHEMA.to_string(),
            handoff_id: mailbox.handoff_id,
            pairing_link: mailbox.pairing_link,
            expected_ca_fingerprint: mailbox.expected_ca_fingerprint,
            expected_instance_id: mailbox.expected_instance_id,
            accepted_unix_ms: 1_000,
        };
        let path = dir.path().join(PROGRESS_FILE_NAME);
        let valid = serde_json::to_value(&progress).unwrap();
        fs::write(&path, serde_json::to_vec(&valid).unwrap()).unwrap();
        assert_eq!(read_progress(dir.path()).unwrap(), Some(progress));

        // Exactly 64 bytes, with a character spanning the old byte-32 slice.
        let non_ascii_fingerprint = format!("{}é{}", "a".repeat(31), "a".repeat(31));
        assert_eq!(non_ascii_fingerprint.len(), 64);
        for (field, bad) in [
            ("schema", "unknown".to_string()),
            ("handoff_id", "not-a-uuid".to_string()),
            ("expected_ca_fingerprint", non_ascii_fingerprint),
            ("expected_ca_fingerprint", "g".repeat(64)),
            ("expected_ca_fingerprint", SAMPLE_FP.to_uppercase()),
            ("expected_instance_id", String::new()),
            ("pairing_link", "https://example.invalid/#x".to_string()),
        ] {
            let mut corrupt = valid.clone();
            corrupt[field] = serde_json::Value::String(bad);
            fs::write(&path, serde_json::to_vec(&corrupt).unwrap()).unwrap();
            assert_eq!(
                read_progress(dir.path()).unwrap_err().kind(),
                io::ErrorKind::InvalidData,
                "corrupt {field} must not become an accepted continuation"
            );
        }
    }

    #[test]
    fn admission_owner_alone_releases_the_flag_and_validation_failure_clears_it() {
        let dir = TestDir::new("admission-owner");
        let active = AtomicBool::new(false);
        let (guard, ()) = claim_pairing_admission(dir.path(), &active, || Ok(())).unwrap();
        assert!(active.load(Ordering::Acquire));
        assert!(claim_pairing_admission(dir.path(), &active, || Ok(())).is_err());
        assert!(active.load(Ordering::Acquire));
        drop(guard);
        assert!(!active.load(Ordering::Acquire));
        let failed: Result<(PairingAdmission<'_>, ()), _> =
            claim_pairing_admission(dir.path(), &active, || {
                Err(crate::TransportError::Pairing("refused".into()))
            });
        assert!(failed.is_err());
        assert!(!active.load(Ordering::Acquire));
    }

    #[test]
    fn mailbox_admission_observes_the_live_claim_and_retries_after_release() {
        let dir = TestDir::new("admission-live");
        let active = AtomicBool::new(false);
        let (guard, ()) = claim_pairing_admission(dir.path(), &active, || Ok(())).unwrap();
        write_mailbox(dir.path(), &sample_mailbox(ID_A));
        assert_eq!(
            process_mailbox_live(dir.path(), 1_000, || active.load(Ordering::Acquire)).unwrap(),
            MailboxOutcome::Deferred
        );
        assert!(read_progress(dir.path()).unwrap().is_none());
        assert!(!dir.path().join(RECEIPT_FILE_NAME).exists());
        drop(guard);
        assert_eq!(
            process_mailbox_live(dir.path(), 2_000, || active.load(Ordering::Acquire)).unwrap(),
            MailboxOutcome::Accepted
        );
    }

    #[test]
    fn ordinary_submit_classifier_is_absent() {
        let source = include_str!("handoff.rs");
        let needle = ["classify", "_submit"].concat();
        assert!(!source.contains(&needle));
        assert!(!source.contains(&["Submit", "Route"].concat()));
    }
}
