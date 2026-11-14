// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! A per-user journal target shared by scheme launches and the running app.

use crate::handoff::{acquire_handoff_lock, read_bounded, write_atomic_json, HandoffLock};
use serde::{Deserialize, Serialize};
use std::{io, path::Path};

const FILE: &str = "journal-target.json";
const MAX_BYTES: usize = 1024;

#[derive(Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Pending {
    pub target: Option<String>,
    pub invalid_notice: bool,
}

pub fn read(root: &Path) -> io::Result<Pending> {
    let bytes = match read_bounded(&root.join(FILE), MAX_BYTES) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Pending::default()),
        Err(error) => return Err(error),
    };
    let pending: Pending = serde_json::from_slice(&bytes)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "invalid journal target file"))?;
    if pending
        .target
        .as_deref()
        .is_some_and(|t| !observer_model::journal_target::valid_target(t))
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid journal target file",
        ));
    }
    Ok(pending)
}

fn update(root: &Path, change: impl FnOnce(&mut Pending)) -> io::Result<()> {
    let _lock = match acquire_handoff_lock(root)? {
        HandoffLock::Held(lock) => lock,
        HandoffLock::Busy => {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "journal target busy",
            ))
        }
    };
    let mut pending = read(root)?;
    change(&mut pending);
    write_atomic_json(&root.join(FILE), &pending, MAX_BYTES)
}

/// `None` requests the invalid-link notice without replacing a held valid link.
pub fn store(root: &Path, target: Option<&str>) -> io::Result<()> {
    if target.is_some_and(|t| !observer_model::journal_target::valid_target(t)) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid journal target",
        ));
    }
    update(root, |pending| {
        if let Some(target) = target {
            pending.target = Some(target.to_owned());
        }
        pending.invalid_notice = target.is_none();
    })
}

pub fn take_invalid_notice(root: &Path) -> io::Result<bool> {
    if !read(root)?.invalid_notice {
        return Ok(false);
    }
    let mut notice = false;
    update(root, |pending| {
        notice = pending.invalid_notice;
        pending.invalid_notice = false;
    })?;
    Ok(notice)
}

pub fn clear_used(root: &Path, used: &str) -> io::Result<()> {
    update(root, |pending| {
        if pending.target.as_deref() == Some(used) {
            pending.target = None;
        }
    })
}

#[cfg(all(test, feature = "transport-tests"))]
mod tests {
    use super::*;

    #[test]
    fn held_link_survives_notice_and_a_newer_arrival_survives_consumption() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        assert!(read(root).unwrap().target.is_none());
        store(root, Some("Aa-_~090020")).unwrap();
        store(root, None).unwrap();
        assert!(take_invalid_notice(root).unwrap());
        assert!(!take_invalid_notice(root).unwrap());
        assert_eq!(read(root).unwrap().target.as_deref(), Some("Aa-_~090020"));
        store(root, Some("Bb")).unwrap();
        clear_used(root, "Aa-_~090020").unwrap();
        assert_eq!(read(root).unwrap().target.as_deref(), Some("Bb"));
        clear_used(root, "Bb").unwrap();
        assert!(read(root).unwrap().target.is_none());
    }

    #[test]
    fn failed_or_busy_store_preserves_the_incumbent_and_bounded_reads_refuse_corruption() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        store(root, Some("incumbent")).unwrap();
        assert!(store(root, Some("bad/")).is_err());
        let held = match acquire_handoff_lock(root).unwrap() {
            HandoffLock::Held(lock) => lock,
            HandoffLock::Busy => panic!("unexpected busy lock"),
        };
        assert_eq!(
            store(root, Some("later")).unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
        drop(held);
        assert_eq!(read(root).unwrap().target.as_deref(), Some("incumbent"));
        std::fs::write(root.join(FILE), vec![b'a'; MAX_BYTES + 1]).unwrap();
        assert_eq!(read(root).err().unwrap().kind(), io::ErrorKind::InvalidData);
    }
}
