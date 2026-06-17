// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! The sealed-segment source the uploader drains.
//!
//! W1 seals each finished segment into `…/segments/<index>/` (renamed from
//! `<index>.incomplete`) holding the per-source files (`display_1_screen.mp4`,
//! `system-audio.pcm`, `mic.pcm`). The clock-aligned boundary is
//! `index * period_secs` epoch seconds, which the uploader turns into the
//! journal's `day` / `segment` keys. Behind a trait so the coordinator is
//! host-testable with a temp dir.

use std::path::{Path, PathBuf};

/// A sealed segment ready to upload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SealedSegment {
    /// The sealed dir's numeric name = the clock-boundary index.
    pub index: u64,
    /// `index * period_secs` — the segment's aligned start instant (epoch secs).
    pub boundary_epoch_secs: u64,
    /// File names inside the segment dir.
    pub files: Vec<String>,
}

/// Source of sealed segments. Real impl scans `%LocalAppData%`; tests use a
/// temp dir.
pub trait SealedStore: Send + Sync {
    fn scan(&self) -> std::io::Result<Vec<SealedSegment>>;
    fn read_file(&self, index: u64, name: &str) -> std::io::Result<Vec<u8>>;
    fn remove(&self, index: u64) -> std::io::Result<()>;
}

/// The best-effort content type for an observer segment file. The journal stores
/// by filename and globs the pipeline by name, so this is advisory.
pub fn content_type_for(name: &str) -> String {
    match name {
        "system-audio.pcm" | "mic.pcm" => "audio/L16".to_string(),
        name if name.ends_with(".mp4") => "video/mp4".to_string(),
        _ => "application/octet-stream".to_string(),
    }
}

/// Filesystem-backed sealed store over the segments root.
pub struct LocalSealedStore {
    root: PathBuf,
    period_secs: u64,
}

impl LocalSealedStore {
    pub fn new(root: impl Into<PathBuf>, period_secs: u64) -> Self {
        Self {
            root: root.into(),
            period_secs: period_secs.max(1),
        }
    }

    fn segment_dir(&self, index: u64) -> PathBuf {
        self.root.join(index.to_string())
    }
}

fn parse_sealed_index(name: &str) -> Option<u64> {
    // Sealed dirs are pure decimal; `<n>.incomplete`, `quarantine`, etc. won't
    // parse and are skipped.
    if name.chars().all(|c| c.is_ascii_digit()) && !name.is_empty() {
        name.parse::<u64>().ok()
    } else {
        None
    }
}

fn list_files(dir: &Path) -> std::io::Result<Vec<String>> {
    let mut files = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        if entry.file_type()?.is_file() {
            if let Some(name) = entry.file_name().to_str() {
                files.push(name.to_string());
            }
        }
    }
    files.sort();
    Ok(files)
}

impl SealedStore for LocalSealedStore {
    fn scan(&self) -> std::io::Result<Vec<SealedSegment>> {
        let mut out = Vec::new();
        let dir = match std::fs::read_dir(&self.root) {
            Ok(d) => d,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(out),
            Err(e) => return Err(e),
        };
        for entry in dir {
            let entry = entry?;
            if !entry.file_type()?.is_dir() {
                continue;
            }
            let Some(name) = entry.file_name().to_str().map(str::to_string) else {
                continue;
            };
            let Some(index) = parse_sealed_index(&name) else {
                continue;
            };
            let files = list_files(&entry.path())?;
            out.push(SealedSegment {
                index,
                boundary_epoch_secs: index.saturating_mul(self.period_secs),
                files,
            });
        }
        out.sort_by_key(|s| s.index);
        Ok(out)
    }

    fn read_file(&self, index: u64, name: &str) -> std::io::Result<Vec<u8>> {
        std::fs::read(self.segment_dir(index).join(name))
    }

    fn remove(&self, index: u64) -> std::io::Result<()> {
        std::fs::remove_dir_all(self.segment_dir(index))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use observer_model::SCREEN_FILE_NAME;

    fn temp_root() -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "plw-sealed-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    #[test]
    fn scans_sealed_dirs_skips_incomplete() {
        let root = temp_root();
        // sealed segment index 7 with two files
        let seg = root.join("7");
        std::fs::create_dir_all(&seg).unwrap();
        std::fs::write(seg.join(SCREEN_FILE_NAME), b"MP4").unwrap();
        std::fs::write(seg.join("system-audio.pcm"), b"PCM").unwrap();
        // an in-flight dir that must be ignored
        std::fs::create_dir_all(root.join("8.incomplete")).unwrap();
        // a stray non-numeric dir
        std::fs::create_dir_all(root.join("quarantine")).unwrap();

        let store = LocalSealedStore::new(&root, 300);
        let segs = store.scan().unwrap();
        assert_eq!(segs.len(), 1);
        assert_eq!(segs[0].index, 7);
        assert_eq!(segs[0].boundary_epoch_secs, 7 * 300);
        assert_eq!(segs[0].files, vec![SCREEN_FILE_NAME, "system-audio.pcm"]);
        assert_eq!(store.read_file(7, SCREEN_FILE_NAME).unwrap(), b"MP4");

        store.remove(7).unwrap();
        assert!(store.scan().unwrap().is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn mp4_content_type_is_video_mp4() {
        assert_eq!(content_type_for(SCREEN_FILE_NAME), "video/mp4");
    }

    #[test]
    fn coordinator_uploads_renamed_mp4_filename_unchanged() {
        let root = temp_root();
        let seg = root.join("9");
        std::fs::create_dir_all(&seg).unwrap();
        std::fs::write(seg.join(SCREEN_FILE_NAME), b"MP4").unwrap();

        let store = LocalSealedStore::new(&root, 300);
        let segs = store.scan().unwrap();

        assert_eq!(segs.len(), 1);
        assert_eq!(segs[0].files, vec![SCREEN_FILE_NAME]);
        assert_eq!(store.read_file(9, &segs[0].files[0]).unwrap(), b"MP4");

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn missing_root_scans_empty() {
        let store = LocalSealedStore::new(std::env::temp_dir().join("plw-nope-zzz"), 300);
        assert!(store.scan().unwrap().is_empty());
    }
}
