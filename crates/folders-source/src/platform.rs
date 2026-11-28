// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Platform seam for the folder source: clock, local UTC offset, home folder,
//! per-directory stable file IDs, NTFS `Zone.Identifier` reads, Windows file
//! attribute checks, screenshot recognition, and on-device image conversion.

use std::collections::HashMap;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::stamp::Stamp;
use crate::types::FileKind;
use crate::zone_identifier::ZoneIdentifier;

/// Converted image bytes ready to attach as a companion upload part.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConvertedImage {
    pub bytes: Vec<u8>,
    pub extension: &'static str,
    pub content_type: &'static str,
}

/// Platform hooks supplied by the host app (on Windows, `platform-win`).
pub trait FolderPlatform: Send + Sync + 'static {
    /// Current wall-clock time in UTC epoch milliseconds.
    fn now_ms(&self) -> i64 {
        match SystemTime::now().duration_since(UNIX_EPOCH) {
            Ok(d) => i64::try_from(d.as_millis()).unwrap_or(i64::MAX),
            Err(_) => 0,
        }
    }

    /// Device-local UTC offset (seconds east of UTC) for the given UTC epoch second.
    fn offset_s_at(&self, epoch_secs: i64) -> i32;

    /// Current timestamp paired with its local UTC offset.
    fn now_stamp(&self) -> Stamp {
        let ms = self.now_ms();
        Stamp::new(ms, self.offset_s_at(ms.div_euclid(1_000)))
    }

    /// Timestamp paired with the local UTC offset that applied at `ms`.
    fn stamp_at(&self, ms: i64) -> Stamp {
        Stamp::new(ms, self.offset_s_at(ms.div_euclid(1_000)))
    }

    /// User's home directory (`USERPROFILE` on Windows), for friendly path labels.
    fn home_dir(&self) -> PathBuf;

    /// Map filename -> stable file identity string for entries in `dir`,
    /// without opening individual files (placeholder-safe on NTFS/ReFS).
    fn file_ids_in_dir(&self, _dir: &Path) -> HashMap<OsString, String> {
        HashMap::new()
    }

    /// Read NTFS `:Zone.Identifier` metadata for a local (non-placeholder) file.
    /// Callers MUST verify `!is_placeholder(&meta)` before calling this.
    fn read_zone_identifier(&self, _path: &Path) -> Option<ZoneIdentifier> {
        None
    }

    /// Whether directory entry metadata marks a hidden or system entry on Windows.
    fn is_hidden_or_system(&self, _meta: &fs::Metadata) -> bool {
        false
    }

    /// Whether directory entry metadata marks a Windows reparse point (junction/symlink).
    fn is_reparse_point(&self, _meta: &fs::Metadata) -> bool {
        false
    }

    /// Whether an already-confirmed-local image file is a Windows screenshot
    /// (inside Windows' Screenshots folder, Snipping Tool / legacy filename,
    /// or Game Bar PNG marker).
    fn is_screenshot(&self, _path: &Path, _extension: Option<&str>) -> bool {
        false
    }

    /// Convert a `convert_to_png_on_device` image (HEIC/HEIF/TIFF/GIF/BMP)
    /// into PNG (for screenshots) or JPEG/PNG (for photos).
    fn convert_image(&self, _bytes: &[u8], _kind: FileKind) -> Option<ConvertedImage> {
        None
    }
}
