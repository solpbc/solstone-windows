// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! What the source keeps between runs: the owner's choices and every
//! screenshot it has taken in, with what became of each. One JSON file, written
//! whole and moved into place, so a crash leaves the last good copy.

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::rules::{Span, TakenFrom};

pub const FILE_NAME: &str = "ledger.json";
const VERSION: u32 = 1;

/// One screenshot as a listing found it. `id` is the sha256 of its bytes, or
/// `path:<path>` when it couldn't be read, or `cloud:<path>` for a file that is
/// only in the cloud and is read when it is sent.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
#[serde(default)]
pub struct Shot {
    pub id: String,
    pub path: String,
    /// The capture second, as an instant.
    pub captured_at: Option<i64>,
    pub taken_from: Option<TakenFrom>,
    pub bytes: u64,
    /// Lowercase extension.
    pub ext: String,
    pub pixels: Option<u64>,
    pub cloud_only: bool,
}

impl Shot {
    pub fn unread(&self) -> bool {
        self.id.starts_with("path:")
    }

    pub fn has_hash(&self) -> bool {
        !self.id.starts_with("path:") && !self.id.starts_with("cloud:")
    }
}

/// What became of one screenshot.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    #[default]
    Pending,
    Delivered,
    /// The journal won't keep it, or it can't be converted. Final.
    Refused,
    /// It couldn't be read. Read again on every look.
    Unreadable,
    /// Gone from the folder before it was sent.
    Deleted,
    /// Sent and waiting for a better answer.
    Held,
}

impl Outcome {
    pub fn waiting(self) -> bool {
        matches!(self, Outcome::Pending | Outcome::Held)
    }
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
#[serde(default)]
pub struct Item {
    #[serde(flatten)]
    pub shot: Shot,
    /// Part of what the owner confirmed bringing in from before.
    pub frozen: bool,
    pub outcome: Outcome,
}

/// A stretch of time paused. `end` is exclusive: the moment of resume comes in.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default)]
pub struct PauseWindow {
    pub start: i64,
    pub end: Option<i64>,
    /// When the pause was set to end on its own, if it was.
    pub expiration: Option<i64>,
}

impl PauseWindow {
    pub fn excludes(&self, at: i64) -> bool {
        at >= self.start && self.end.is_none_or(|end| at < end)
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(default)]
pub struct Ledger {
    pub v: u32,
    /// The owner turned this on at least once, or chose a folder.
    pub consented: bool,
    /// The switch.
    pub enabled: bool,
    /// The owner confirmed how far back.
    pub confirmed: bool,
    /// It was on before; turning it back on says what's already in.
    pub was_on: bool,
    /// The folder the owner chose; none means the one Windows resolves.
    pub chosen: Option<String>,
    /// The folder has been listed at least once, so its going away is news.
    pub folder_seen: bool,
    pub span: Span,
    /// Where the confirmed span began: nothing older becomes a new item.
    pub live_from: Option<i64>,
    pub items: Vec<Item>,
    pub pause_windows: Vec<PauseWindow>,
    pub last_checked_ms: Option<i64>,
    /// What the last listing found, so a confirm freezes what was counted.
    pub seen: Vec<Shot>,
}

impl Default for Ledger {
    fn default() -> Self {
        Self {
            v: VERSION,
            consented: false,
            enabled: false,
            confirmed: false,
            was_on: false,
            chosen: None,
            folder_seen: false,
            span: Span::default(),
            live_from: None,
            items: Vec::new(),
            pause_windows: Vec::new(),
            last_checked_ms: None,
            seen: Vec::new(),
        }
    }
}

impl Ledger {
    /// Read the ledger in `dir`. A missing file is a fresh source; a file that
    /// doesn't read is an error, so it is never silently replaced.
    pub fn load(dir: &Path) -> io::Result<Self> {
        match fs::read(dir.join(FILE_NAME)) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Self::default()),
            Err(error) => Err(error),
        }
    }

    /// Write the whole ledger beside itself, then move it into place.
    pub fn save(&self, dir: &Path) -> io::Result<()> {
        fs::create_dir_all(dir)?;
        let path = dir.join(FILE_NAME);
        let tmp: PathBuf = dir.join(format!("{FILE_NAME}.tmp"));
        let bytes = serde_json::to_vec(self).map_err(io::Error::other)?;
        {
            let mut file = fs::File::create(&tmp)?;
            file.write_all(&bytes)?;
            file.sync_all()?;
        }
        fs::rename(&tmp, &path)
    }

    pub fn in_pause_window(&self, at: i64) -> bool {
        self.pause_windows.iter().any(|w| w.excludes(at))
    }
}
