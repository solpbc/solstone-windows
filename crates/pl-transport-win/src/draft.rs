// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Desktop pair-link draft and deferred-link files under the handoff lock.

use std::fs::{self, File};
use std::io::{self, Write};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use observer_contract::{
    DEFERRED_FILE_NAME, DEFERRED_REJECTED_FILE_NAME, DRAFT_FILE_NAME, DRAFT_MAX_BYTES, DRAFT_SCHEMA,
};
use serde::{Deserialize, Serialize};

use crate::handoff::{acquire_handoff_lock, read_bounded, HandoffLock};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PairLinkDraft {
    pub schema: String,
    pub pairing_link: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DraftWriteOutcome {
    Stored,
    Deferred,
    Rejected,
    Busy,
}

fn read_link_at(path: &Path) -> Option<String> {
    let bytes = read_bounded(path, DRAFT_MAX_BYTES).ok()?;
    let draft: PairLinkDraft = serde_json::from_slice(&bytes).ok()?;
    if draft.schema == DRAFT_SCHEMA && !draft.pairing_link.is_empty() {
        Some(draft.pairing_link)
    } else {
        None
    }
}

pub fn read_draft(data_root: &Path) -> Option<String> {
    read_link_at(&data_root.join(DRAFT_FILE_NAME))
}

pub fn read_deferred(data_root: &Path) -> Option<String> {
    read_link_at(&data_root.join(DEFERRED_FILE_NAME))
}

pub fn deferred_rejected(data_root: &Path) -> bool {
    data_root.join(DEFERRED_REJECTED_FILE_NAME).is_file()
}

fn held_lock(data_root: &Path) -> io::Result<Option<File>> {
    match acquire_handoff_lock(data_root)? {
        HandoffLock::Held(file) => Ok(Some(file)),
        HandoffLock::Busy => Ok(None),
    }
}

fn write_link_file(path: &Path, link: &str) -> io::Result<()> {
    let draft = PairLinkDraft {
        schema: DRAFT_SCHEMA.to_string(),
        pairing_link: link.to_string(),
    };
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::other("missing parent directory"))?;
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("draft");
    let tmp = parent.join(format!(
        "{file_name}.{}.{}.tmp",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    ));
    let bytes = serde_json::to_vec_pretty(&draft)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    if bytes.len() > DRAFT_MAX_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "draft exceeds maximum bytes",
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

fn clear_rejected(data_root: &Path) {
    let _ = fs::remove_file(data_root.join(DEFERRED_REJECTED_FILE_NAME));
}

/// Store a desktop link, or keep the incumbent and record one deferred link.
pub fn write_draft_if_allowed(
    data_root: &Path,
    link: &str,
    is_paired: bool,
    ceremony_active: bool,
) -> io::Result<DraftWriteOutcome> {
    let Some(_lock) = held_lock(data_root)? else {
        return Ok(DraftWriteOutcome::Busy);
    };

    let draft_path = data_root.join(DRAFT_FILE_NAME);
    let deferred_path = data_root.join(DEFERRED_FILE_NAME);
    let incumbent = is_paired || ceremony_active || draft_path.exists();
    if !incumbent {
        write_link_file(&draft_path, link)?;
        clear_rejected(data_root);
        return Ok(DraftWriteOutcome::Stored);
    }

    if !deferred_path.exists() {
        write_link_file(&deferred_path, link)?;
        clear_rejected(data_root);
        return Ok(DraftWriteOutcome::Deferred);
    }

    let rejected = data_root.join(DEFERRED_REJECTED_FILE_NAME);
    if !rejected.exists() {
        File::create(&rejected)?;
    }
    Ok(DraftWriteOutcome::Rejected)
}

pub fn take_deferred(data_root: &Path) -> io::Result<Option<String>> {
    let Some(_lock) = held_lock(data_root)? else {
        return Err(io::Error::new(
            io::ErrorKind::WouldBlock,
            "handoff lock busy",
        ));
    };
    let path = data_root.join(DEFERRED_FILE_NAME);
    let link = read_link_at(&path);
    if let Some(link) = link.as_ref() {
        write_link_file(&data_root.join(DRAFT_FILE_NAME), link)?;
    }
    if path.exists() {
        fs::remove_file(&path)?;
    }
    Ok(link)
}

pub fn dismiss_deferred(data_root: &Path) -> io::Result<()> {
    let Some(_lock) = held_lock(data_root)? else {
        return Err(io::Error::new(
            io::ErrorKind::WouldBlock,
            "handoff lock busy",
        ));
    };
    match fs::remove_file(data_root.join(DEFERRED_FILE_NAME)) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

pub fn dismiss_rejected(data_root: &Path) -> io::Result<()> {
    let Some(_lock) = held_lock(data_root)? else {
        return Err(io::Error::new(
            io::ErrorKind::WouldBlock,
            "handoff lock busy",
        ));
    };
    match fs::remove_file(data_root.join(DEFERRED_REJECTED_FILE_NAME)) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_root(name: &str) -> std::path::PathBuf {
        let base = if Path::new("/var/tmp").exists() {
            std::path::PathBuf::from("/var/tmp")
        } else {
            std::env::temp_dir()
        };
        let path = base.join(format!(
            "draft-test-{name}-{}-{}",
            std::process::id(),
            observer_model::monotonic_us()
        ));
        path
    }

    #[test]
    fn incumbent_draft_is_kept_and_a_deferred_file_is_written() {
        let root = test_root("incumbent");
        let link = "https://go.solstone.app/p#0G0W000258DSX8DJRFAEBXG7308J4CT4ANK7F26YNPZEZJQYQAZ028T5CY4TQKFF";
        let later = "https://go.solstone.app/p#0R0J6HB7H6NWVVR1VTPVXVYAZTXBW0938NKRKAYDXWAPGX3ME1SKMBSFE9JPRRBS5SJQGRBDE1P6A";
        assert_eq!(
            write_draft_if_allowed(&root, link, false, false).unwrap(),
            DraftWriteOutcome::Stored
        );
        assert_eq!(read_draft(&root).as_deref(), Some(link));
        assert_eq!(
            write_draft_if_allowed(&root, later, false, false).unwrap(),
            DraftWriteOutcome::Deferred
        );
        assert_eq!(read_draft(&root).as_deref(), Some(link));
        assert_eq!(read_deferred(&root).as_deref(), Some(later));
        assert!(!deferred_rejected(&root));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn second_deferred_arrival_does_not_overwrite_and_sets_the_marker() {
        let root = test_root("second");
        let first = "https://go.solstone.app/p#0G0W000258DSX8DJRFAEBXG7308J4CT4ANK7F26YNPZEZJQYQAZ028T5CY4TQKFF";
        let second = "https://go.solstone.app/p#0R0J6HB7H6NWVVR1VTPVXVYAZTXBW0938NKRKAYDXWAPGX3ME1SKMBSFE9JPRRBS5SJQGRBDE1P6A";
        let third = "https://go.solstone.app/p#0G0W000258DSX8DJRFAEBXG7308J4CT4ANK7F26YNPZEZJQYQAZ028T5CY4TQKEE";
        write_draft_if_allowed(&root, first, false, false).unwrap();
        write_draft_if_allowed(&root, second, true, false).unwrap();
        assert_eq!(
            write_draft_if_allowed(&root, third, true, false).unwrap(),
            DraftWriteOutcome::Rejected
        );
        assert_eq!(read_deferred(&root).as_deref(), Some(second));
        assert!(deferred_rejected(&root));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn explicit_selection_persists_the_deferred_link_before_consuming_it() {
        let root = test_root("promote");
        write_draft_if_allowed(&root, "ordinary-first", false, false).unwrap();
        write_draft_if_allowed(&root, "ordinary-later", false, false).unwrap();
        assert_eq!(
            take_deferred(&root).unwrap().as_deref(),
            Some("ordinary-later")
        );
        assert_eq!(read_draft(&root).as_deref(), Some("ordinary-later"));
        assert!(read_deferred(&root).is_none());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn failed_selection_keeps_the_pending_link_for_retry() {
        let root = test_root("promote-failure");
        write_draft_if_allowed(&root, "ordinary-first", false, false).unwrap();
        write_draft_if_allowed(&root, "ordinary-later", false, false).unwrap();
        let pending = fs::read(root.join(DEFERRED_FILE_NAME)).unwrap();
        fs::remove_file(root.join(DRAFT_FILE_NAME)).unwrap();
        fs::create_dir(root.join(DRAFT_FILE_NAME)).unwrap();
        assert!(take_deferred(&root).is_err());
        assert_eq!(fs::read(root.join(DEFERRED_FILE_NAME)).unwrap(), pending);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn unique_temp_is_not_the_fixed_draft_tmp() {
        let root = test_root("temp");
        fs::create_dir_all(&root).unwrap();
        let sentinel = root.join(format!("{DRAFT_FILE_NAME}.tmp"));
        let mut file = File::create(&sentinel).unwrap();
        file.write_all(b"SENTINEL").unwrap();
        let link = "https://go.solstone.app/p#0G0W000258DSX8DJRFAEBXG7308J4CT4ANK7F26YNPZEZJQYQAZ028T5CY4TQKFF";
        assert_eq!(
            write_draft_if_allowed(&root, link, false, false).unwrap(),
            DraftWriteOutcome::Stored
        );
        assert_eq!(fs::read(&sentinel).unwrap(), b"SENTINEL");
        assert_eq!(read_draft(&root).as_deref(), Some(link));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn busy_lock_writes_nothing() {
        let root = test_root("busy");
        fs::create_dir_all(&root).unwrap();
        let _held = match acquire_handoff_lock(&root).unwrap() {
            HandoffLock::Held(file) => file,
            HandoffLock::Busy => panic!("lock should be free"),
        };
        let link = "https://go.solstone.app/p#0G0W000258DSX8DJRFAEBXG7308J4CT4ANK7F26YNPZEZJQYQAZ028T5CY4TQKFF";
        assert_eq!(
            write_draft_if_allowed(&root, link, false, false).unwrap(),
            DraftWriteOutcome::Busy
        );
        assert!(read_draft(&root).is_none());
        assert!(read_deferred(&root).is_none());
        assert!(!root.join(DRAFT_FILE_NAME).exists());
        assert!(!root.join(DEFERRED_FILE_NAME).exists());
        drop(_held);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn missing_root_is_created() {
        let root = test_root("mkdir").join("nested");
        assert!(!root.exists());
        let link = "https://go.solstone.app/p#0G0W000258DSX8DJRFAEBXG7308J4CT4ANK7F26YNPZEZJQYQAZ028T5CY4TQKFF";
        assert_eq!(
            write_draft_if_allowed(&root, link, false, false).unwrap(),
            DraftWriteOutcome::Stored
        );
        assert!(root.is_dir());
        assert_eq!(read_draft(&root).as_deref(), Some(link));
        let _ = fs::remove_dir_all(root.parent().unwrap());
    }

    #[test]
    fn legacy_link_field_is_not_a_draft() {
        let root = test_root("legacy");
        fs::create_dir_all(&root).unwrap();
        fs::write(
            root.join(DRAFT_FILE_NAME),
            br#"{"schema":"solstone.pair-link-draft.v1","link":"https://go.solstone.app/p#abc"}"#,
        )
        .unwrap();
        assert!(read_draft(&root).is_none());
        let _ = fs::remove_dir_all(&root);
    }
}
