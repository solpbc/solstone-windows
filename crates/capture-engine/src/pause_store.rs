// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! The owner's pause, held across restarts.
//!
//! A pause the owner chose lasts until the owner resumes or its own deadline
//! passes. Quitting, a crash, an update, signing out or a restart does not end
//! it. A lock or sleep pause is not an owner pause and is never recorded.

use std::fs;
use std::io::{self, Write};
use std::path::PathBuf;

/// A pause the owner chose and has not ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeldPause {
    /// "until I resume".
    UntilResumed,
    /// A timed pause ending at this wall-clock time (Unix seconds).
    Until(u64),
}

/// Where the engine keeps the owner's pause between runs.
pub trait PauseStore: Send {
    /// The held pause, if any.
    fn load(&self) -> Option<HeldPause>;
    /// Record the pause durably before returning.
    fn save(&self, pause: HeldPause) -> io::Result<()>;
    /// Remove the held pause. Absent is already clear.
    fn clear(&self) -> io::Result<()>;
}

/// A one-line text file: `until-resumed` or `until <unix seconds>`.
pub struct FilePauseStore {
    path: PathBuf,
}

impl FilePauseStore {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }
}

const UNTIL_RESUMED: &str = "until-resumed";
const UNTIL: &str = "until ";

fn encode(pause: HeldPause) -> String {
    match pause {
        HeldPause::UntilResumed => format!("{UNTIL_RESUMED}\n"),
        HeldPause::Until(deadline) => format!("{UNTIL}{deadline}\n"),
    }
}

/// A file that exists but does not parse is "until I resume": staying paused
/// shows the paused state and the owner can resume, while capturing against
/// the owner's choice cannot be undone.
fn decode(text: &str) -> HeldPause {
    let line = text.trim();
    if line == UNTIL_RESUMED {
        return HeldPause::UntilResumed;
    }
    match line.strip_prefix(UNTIL).and_then(|n| n.trim().parse().ok()) {
        Some(deadline) => HeldPause::Until(deadline),
        None => {
            tracing::warn!(target: "engine", "held pause unreadable; staying paused");
            HeldPause::UntilResumed
        }
    }
}

impl PauseStore for FilePauseStore {
    fn load(&self) -> Option<HeldPause> {
        match fs::read(&self.path) {
            Ok(bytes) => Some(decode(&String::from_utf8_lossy(&bytes))),
            Err(error) if error.kind() == io::ErrorKind::NotFound => None,
            Err(error) => {
                tracing::warn!(target: "engine", %error, "held pause unreadable; staying paused");
                Some(HeldPause::UntilResumed)
            }
        }
    }

    fn save(&self, pause: HeldPause) -> io::Result<()> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }
        let temporary = self.path.with_extension("tmp");
        {
            let mut file = fs::File::create(&temporary)?;
            file.write_all(encode(pause).as_bytes())?;
            file.sync_all()?;
        }
        fs::rename(&temporary, &self.path)
    }

    fn clear(&self) -> io::Result<()> {
        match fs::remove_file(&self.path) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            result => result,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_store(name: &str) -> (PathBuf, FilePauseStore) {
        let dir = std::env::temp_dir().join(format!(
            "solstone-pause-store-{name}-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        let path = dir.join("pause.txt");
        (dir, FilePauseStore::new(path))
    }

    #[test]
    fn round_trips_both_kinds_and_clears() {
        let (dir, store) = temp_store("round-trip");
        assert_eq!(store.load(), None);
        store.clear().unwrap();
        store.save(HeldPause::UntilResumed).unwrap();
        assert_eq!(store.load(), Some(HeldPause::UntilResumed));
        store.save(HeldPause::Until(1_900)).unwrap();
        assert_eq!(store.load(), Some(HeldPause::Until(1_900)));
        store.clear().unwrap();
        assert_eq!(store.load(), None);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn an_unreadable_file_stays_paused() {
        for text in ["", "garbage", "until", "until soon", "until -5"] {
            assert_eq!(decode(text), HeldPause::UntilResumed, "{text:?}");
        }
        assert_eq!(decode("until 1900\n"), HeldPause::Until(1_900));
    }
}
