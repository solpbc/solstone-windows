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

/// Marker file written into a sealed dir once its upload is confirmed and the
/// owner's retention policy says to keep the local copy. Its presence means the
/// segment is **done uploading** — [`SealedStore::scan`] skips it (never
/// re-uploads) and it lives until the retention window prunes it.
pub const UPLOADED_MARKER: &str = ".uploaded";

/// Source of sealed segments. Real impl scans `%LocalAppData%`; tests use a
/// temp dir.
pub trait SealedStore: Send + Sync {
    /// Sealed segments still pending upload (those **without** the confirmed
    /// marker). Confirmed-but-retained segments are excluded — they are not
    /// re-uploaded.
    fn scan(&self) -> std::io::Result<Vec<SealedSegment>>;
    fn read_file(&self, index: u64, name: &str) -> std::io::Result<Vec<u8>>;
    fn remove(&self, index: u64) -> std::io::Result<()>;
    /// Mark a segment confirmed-uploaded (retain locally). Writes [`UPLOADED_MARKER`].
    fn mark_confirmed(&self, index: u64) -> std::io::Result<()>;
    /// Confirmed-but-retained segments (those **with** the marker), for the
    /// retention prune pass. `files` is not populated — only index + boundary are
    /// needed to decide pruning.
    fn confirmed(&self) -> std::io::Result<Vec<SealedSegment>>;
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

impl LocalSealedStore {
    /// Scan sealed dirs, yielding each as `(index, files)`; the caller filters by
    /// confirmed marker. Shared by `scan` (pending) and `confirmed` (retained).
    fn scan_filtered(&self, want_confirmed: bool) -> std::io::Result<Vec<SealedSegment>> {
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
            let is_confirmed = files.iter().any(|f| f == UPLOADED_MARKER);
            if is_confirmed != want_confirmed {
                continue;
            }
            out.push(SealedSegment {
                index,
                boundary_epoch_secs: index.saturating_mul(self.period_secs),
                files: files.into_iter().filter(|f| f != UPLOADED_MARKER).collect(),
            });
        }
        out.sort_by_key(|s| s.index);
        Ok(out)
    }
}

impl SealedStore for LocalSealedStore {
    fn scan(&self) -> std::io::Result<Vec<SealedSegment>> {
        self.scan_filtered(false)
    }

    fn read_file(&self, index: u64, name: &str) -> std::io::Result<Vec<u8>> {
        std::fs::read(self.segment_dir(index).join(name))
    }

    fn remove(&self, index: u64) -> std::io::Result<()> {
        std::fs::remove_dir_all(self.segment_dir(index))
    }

    fn mark_confirmed(&self, index: u64) -> std::io::Result<()> {
        std::fs::write(self.segment_dir(index).join(UPLOADED_MARKER), [])
    }

    fn confirmed(&self) -> std::io::Result<Vec<SealedSegment>> {
        self.scan_filtered(true)
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

    #[test]
    fn confirmed_marker_excludes_from_scan_and_lists_in_confirmed() {
        let root = temp_root();
        for idx in [3u64, 4] {
            let seg = root.join(idx.to_string());
            std::fs::create_dir_all(&seg).unwrap();
            std::fs::write(seg.join("system-audio.pcm"), b"PCM").unwrap();
        }
        let store = LocalSealedStore::new(&root, 300);

        // Both pending initially; none confirmed.
        assert_eq!(store.scan().unwrap().len(), 2);
        assert!(store.confirmed().unwrap().is_empty());

        // Mark 3 confirmed -> scan (pending) skips it; confirmed lists it, with the
        // marker filtered out of `files` so it's never re-uploaded.
        store.mark_confirmed(3).unwrap();
        let pending = store.scan().unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].index, 4);
        let confirmed = store.confirmed().unwrap();
        assert_eq!(confirmed.len(), 1);
        assert_eq!(confirmed[0].index, 3);
        assert_eq!(confirmed[0].boundary_epoch_secs, 3 * 300);
        assert!(!confirmed[0].files.iter().any(|f| f == UPLOADED_MARKER));
        assert!(confirmed[0].files.contains(&"system-audio.pcm".to_string()));

        // A confirmed segment can still be removed (the prune path).
        store.remove(3).unwrap();
        assert!(store.confirmed().unwrap().is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }
}
