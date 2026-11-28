// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Windows platform hooks for the folder source: local clock/offset,
//! `%USERPROFILE%`, NTFS/ReFS directory file IDs, `:Zone.Identifier` alternate
//! data stream reads (without hydrating cloud placeholders), hidden/system and
//! reparse-point attribute checks, Windows screenshot recognition, and WIC
//! image conversion.

use std::collections::HashMap;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use folders_source::{
    classify::path_starts_with_ci, ConvertedImage, FileKind, FolderPlatform, ZoneIdentifier,
};
use observer_model::LocalOffset;
#[cfg(windows)]
use solstone_folder_sync::files as folder_sync;

#[cfg(windows)]
const FILE_ATTRIBUTE_HIDDEN: u32 = 0x2;
#[cfg(windows)]
const FILE_ATTRIBUTE_SYSTEM: u32 = 0x4;
#[cfg(windows)]
const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
#[cfg(windows)]
const MAX_ZONE_ID_BYTES: u64 = 16 * 1024;

/// Windows implementation of [`FolderPlatform`].
#[derive(Debug, Default)]
pub struct WindowsFolders;

impl FolderPlatform for WindowsFolders {
    fn now_ms(&self) -> i64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
            .unwrap_or(0)
    }

    fn offset_s_at(&self, epoch_secs: i64) -> i32 {
        let Ok(secs) = u64::try_from(epoch_secs) else {
            return 0;
        };
        crate::WindowsLocalOffset
            .local_zone(secs)
            .ok()
            .and_then(|z| i32::try_from(z.utc_offset_seconds).ok())
            .unwrap_or(0)
    }

    fn home_dir(&self) -> PathBuf {
        std::env::var_os("USERPROFILE")
            .or_else(|| std::env::var_os("HOME"))
            .map(PathBuf::from)
            .unwrap_or_else(crate::local_data_root)
    }

    #[cfg(windows)]
    #[allow(unsafe_code)] // Windows final-path lookup on the open file handle.
    fn open_file(&self, root: &Path, path: &Path) -> std::io::Result<fs::File> {
        use std::ffi::OsString;
        use std::io;
        use std::os::windows::ffi::OsStringExt;
        use std::os::windows::fs::OpenOptionsExt;
        use std::os::windows::io::AsRawHandle;
        use windows::Win32::Foundation::HANDLE;
        use windows::Win32::Storage::FileSystem::{
            GetFinalPathNameByHandleW, FILE_FLAG_OPEN_NO_RECALL, FILE_FLAG_OPEN_REPARSE_POINT,
            FILE_SHARE_READ, FILE_SHARE_WRITE, VOLUME_NAME_DOS,
        };

        // Open the reparse point itself. Read only after validating attributes and
        // the final path on this same handle. Keep the alternate stream intact.
        let file = fs::OpenOptions::new()
            .read(true)
            .share_mode((FILE_SHARE_READ | FILE_SHARE_WRITE).0)
            .custom_flags((FILE_FLAG_OPEN_NO_RECALL | FILE_FLAG_OPEN_REPARSE_POINT).0)
            .open(path)?;
        let meta = file.metadata()?;
        if folder_sync::is_placeholder(&meta)
            || self.is_reparse_point(&meta)
            || self.is_hidden_or_system(&meta)
        {
            return Err(io::Error::other("file is not available locally"));
        }
        let handle = HANDLE(file.as_raw_handle());
        let mut name = vec![0; 512];
        let resolved = loop {
            // SAFETY: the file handle and mutable buffer live throughout this call.
            let len =
                unsafe { GetFinalPathNameByHandleW(handle, &mut name, VOLUME_NAME_DOS) } as usize;
            if len == 0 {
                return Err(io::Error::last_os_error());
            }
            if len < name.len() {
                break PathBuf::from(OsString::from_wide(&name[..len]));
            }
            name.resize(len + 1, 0);
        };
        if !folders_source::classify::strip_verbatim_prefix(&resolved).starts_with(root) {
            return Err(io::Error::other("outside selected folder"));
        }
        Ok(file)
    }

    fn file_ids_in_dir(&self, dir: &Path) -> HashMap<OsString, String> {
        crate::dir_file_ids::file_ids_in_dir(dir)
            .into_iter()
            .map(|(name, id)| (OsString::from(name), id))
            .collect()
    }

    fn read_zone_identifier(&self, root: &Path, path: &Path) -> Option<ZoneIdentifier> {
        read_zone_identifier(root, path)
    }

    fn is_hidden_or_system(&self, meta: &fs::Metadata) -> bool {
        #[cfg(windows)]
        {
            use std::os::windows::fs::MetadataExt;
            meta.file_attributes() & (FILE_ATTRIBUTE_HIDDEN | FILE_ATTRIBUTE_SYSTEM) != 0
        }
        #[cfg(not(windows))]
        {
            let _ = meta;
            false
        }
    }

    fn is_reparse_point(&self, meta: &fs::Metadata) -> bool {
        #[cfg(windows)]
        {
            use std::os::windows::fs::MetadataExt;
            meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
        }
        #[cfg(not(windows))]
        {
            let _ = meta;
            false
        }
    }

    fn is_screenshot(&self, root: &Path, path: &Path, extension: Option<&str>) -> bool {
        if let Some(own) = crate::screenshots::own_folder() {
            if path_starts_with_ci(path, &own) {
                return true;
            }
        }
        if path
            .file_stem()
            .and_then(|s| s.to_str())
            .is_some_and(screenshots_source::rules::has_name_stamp)
        {
            return true;
        }
        if extension.is_some_and(|ext| ext.eq_ignore_ascii_case("png")) {
            if let Ok(bytes) = self.read_file(root, path, folders_source::types::MAX_CONTENT_BYTES)
            {
                return screenshots_source::rules::has_game_bar_marker(&bytes);
            }
        }
        false
    }

    fn convert_image(&self, bytes: &[u8], kind: FileKind) -> Option<ConvertedImage> {
        match kind {
            FileKind::Screenshot => {
                crate::screenshots::to_png(bytes)
                    .ok()
                    .map(|out| ConvertedImage {
                        bytes: out,
                        extension: "png",
                        content_type: "image/png",
                    })
            }
            FileKind::Photo => crate::screenshots::to_jpeg(bytes)
                .map(|out| ConvertedImage {
                    bytes: out,
                    extension: "jpg",
                    content_type: "image/jpeg",
                })
                .or_else(|_| {
                    crate::screenshots::to_png(bytes).map(|out| ConvertedImage {
                        bytes: out,
                        extension: "png",
                        content_type: "image/png",
                    })
                })
                .ok(),
            _ => None,
        }
    }
}

/// Read `<path>:Zone.Identifier` on NTFS without ever hydrating a cloud
/// placeholder. Returns `None` when absent, unreadable, or empty.
#[cfg(windows)]
fn read_zone_identifier(root: &Path, path: &Path) -> Option<ZoneIdentifier> {
    use std::io::Read;
    // Never open an alternate stream on a cloud placeholder.
    let sym = fs::symlink_metadata(path).ok()?;
    if folder_sync::is_placeholder(&sym) {
        return None;
    }

    let mut ads = OsString::from(path.as_os_str());
    ads.push(":Zone.Identifier");
    let file = WindowsFolders.open_file(root, Path::new(&ads)).ok()?;
    let mut bytes = Vec::new();
    file.take(MAX_ZONE_ID_BYTES).read_to_end(&mut bytes).ok()?;
    let parsed = folders_source::parse_zone_identifier(&bytes);
    (!parsed.is_empty()).then_some(parsed)
}

#[cfg(not(windows))]
fn read_zone_identifier(_root: &Path, _path: &Path) -> Option<ZoneIdentifier> {
    None
}

#[cfg(all(test, windows))]
mod win_tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn reads_real_ntfs_zone_identifier_ads() {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("downloaded.pdf");
        fs::write(&file_path, b"%PDF-1.7\n").unwrap();

        let mut ads_path = OsString::from(file_path.as_os_str());
        ads_path.push(":Zone.Identifier");
        let mut ads = fs::File::create(Path::new(&ads_path)).unwrap();
        ads.write_all(
            b"[ZoneTransfer]\r\nZoneId=3\r\nReferrerUrl=https://example.org/docs\r\nHostUrl=https://cdn.example.org/downloaded.pdf\r\n",
        )
        .unwrap();
        drop(ads);

        let root = folders_source::classify::normalize_path(dir.path());
        assert_eq!(
            WindowsFolders.read_file(&root, &file_path, 1024).unwrap(),
            b"%PDF-1.7\n"
        );
        let stream = WindowsFolders
            .read_file(&root, Path::new(&ads_path), MAX_ZONE_ID_BYTES)
            .unwrap();
        assert!(stream.starts_with(b"[ZoneTransfer]"));
        let parsed = WindowsFolders
            .read_zone_identifier(&root, &file_path)
            .unwrap();
        assert_eq!(parsed.zone_id, Some(3));
        assert_eq!(
            parsed.origin_url.as_deref(),
            Some("https://cdn.example.org/downloaded.pdf")
        );
        assert_eq!(
            parsed.referrer_url.as_deref(),
            Some("https://example.org/docs")
        );
    }
}
