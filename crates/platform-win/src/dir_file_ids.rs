// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Stable file ids for the entries of one directory, read without opening any of
//! them: a single enumeration of the directory, so a scan never touches a note's
//! data or asks the cloud for a placeholder's.

use std::collections::HashMap;
use std::path::Path;

/// Every entry's stable id in `dir`, by name. Empty on a file system whose ids
/// aren't stable (FAT, exFAT), or when the directory can't be listed.
#[cfg(windows)]
pub fn file_ids_in_dir(dir: &Path) -> HashMap<String, String> {
    imp::read(dir).unwrap_or_default()
}

#[cfg(not(windows))]
pub fn file_ids_in_dir(_dir: &Path) -> HashMap<String, String> {
    HashMap::new()
}

#[cfg(windows)]
mod imp {
    use std::collections::HashMap;
    use std::ffi::OsString;
    use std::fs;
    use std::io;
    use std::os::windows::ffi::OsStringExt;
    use std::os::windows::fs::OpenOptionsExt;
    use std::os::windows::io::AsRawHandle;
    use std::path::Path;

    use windows::Win32::Foundation::{ERROR_NO_MORE_FILES, HANDLE};
    use windows::Win32::Storage::FileSystem::{
        FileIdExtdDirectoryInfo, GetFileInformationByHandleEx, GetVolumeInformationByHandleW,
        FILE_FLAG_BACKUP_SEMANTICS, FILE_LIST_DIRECTORY,
    };

    // Offsets in FILE_ID_EXTD_DIR_INFO.
    const NEXT_ENTRY: usize = 0;
    const CREATION_TIME: usize = 8;
    const NAME_LENGTH: usize = 60;
    const FILE_ID: usize = 72;
    const FILE_NAME: usize = 88;

    /// Whether the file system keeps ids across a rename. NTFS and ReFS do; a FAT
    /// family "id" is a position that moves when the file does.
    fn has_stable_ids(handle: HANDLE) -> bool {
        let mut name = [0u16; 32];
        // SAFETY: the name buffer is live and its length is passed by the slice;
        // the handle is open for this call and every other out pointer is absent.
        let ok = unsafe {
            GetVolumeInformationByHandleW(handle, None, None, None, None, Some(&mut name))
        };
        if ok.is_err() {
            return false;
        }
        let end = name.iter().position(|&c| c == 0).unwrap_or(name.len());
        let name = String::from_utf16_lossy(&name[..end]);
        name.eq_ignore_ascii_case("NTFS") || name.eq_ignore_ascii_case("ReFS")
    }

    pub(super) fn read(dir: &Path) -> io::Result<HashMap<String, String>> {
        let handle = fs::OpenOptions::new()
            .access_mode(FILE_LIST_DIRECTORY.0)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS.0)
            .open(dir)?;
        let raw = HANDLE(handle.as_raw_handle());
        let mut ids = HashMap::new();
        if !has_stable_ids(raw) {
            return Ok(ids);
        }
        // A u64 array keeps the buffer aligned for the 8-byte fields inside it.
        let mut buffer = vec![0u64; 8 * 1024];
        let size = (buffer.len() * 8) as u32;
        loop {
            // SAFETY: the pointer and size describe the live, writable buffer; the
            // handle stays open across the call.
            let listed = unsafe {
                GetFileInformationByHandleEx(
                    raw,
                    FileIdExtdDirectoryInfo,
                    buffer.as_mut_ptr().cast(),
                    size,
                )
            };
            if let Err(error) = listed {
                if error.code() == ERROR_NO_MORE_FILES.to_hresult() {
                    return Ok(ids);
                }
                return Err(error.into());
            }
            // SAFETY: the buffer holds `size` initialized bytes, valid and aligned
            // for the duration of this borrow.
            let bytes =
                unsafe { std::slice::from_raw_parts(buffer.as_ptr().cast::<u8>(), size as usize) };
            collect_entries(bytes, &mut ids);
        }
    }

    /// The file id, then the creation time where the file system has one: a
    /// rename keeps both, and a reused id with a new creation time is another file.
    fn compose(id: &[u8], created: i64) -> String {
        let hex: String = id.iter().map(|b| format!("{b:02x}")).collect();
        if created == 0 {
            hex
        } else {
            format!("{hex}:{created}")
        }
    }

    fn collect_entries(bytes: &[u8], ids: &mut HashMap<String, String>) {
        let mut at = 0usize;
        loop {
            let Some(entry) = bytes.get(at..) else { return };
            let read_u32 = |offset: usize| -> Option<u32> {
                Some(u32::from_le_bytes(
                    entry.get(offset..offset + 4)?.try_into().ok()?,
                ))
            };
            let (Some(next), Some(name_len)) = (read_u32(NEXT_ENTRY), read_u32(NAME_LENGTH)) else {
                return;
            };
            let Some(name_bytes) = entry.get(FILE_NAME..FILE_NAME + name_len as usize) else {
                return;
            };
            let units: Vec<u16> = name_bytes
                .chunks_exact(2)
                .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
                .collect();
            if let (Ok(name), Some(id)) = (
                OsString::from_wide(&units).into_string(),
                entry.get(FILE_ID..FILE_ID + 16),
            ) {
                let created = entry
                    .get(CREATION_TIME..CREATION_TIME + 8)
                    .and_then(|bytes| bytes.try_into().ok())
                    .map_or(0, i64::from_le_bytes);
                ids.insert(name, compose(id, created));
            }
            if next == 0 {
                return;
            }
            at += next as usize;
        }
    }

    #[cfg(test)]
    mod tests {
        use super::super::file_ids_in_dir;
        use std::fs;

        #[test]
        fn a_rename_keeps_the_id_and_a_new_file_gets_its_own() {
            let dir = tempfile::tempdir().unwrap();
            fs::write(dir.path().join("a.md"), "x").unwrap();
            fs::write(dir.path().join("b.md"), "y").unwrap();
            let before = file_ids_in_dir(dir.path());
            let a = before.get("a.md").cloned().expect("a.md has an id");
            let b = before.get("b.md").cloned().expect("b.md has an id");
            assert_ne!(a, b);
            fs::rename(dir.path().join("a.md"), dir.path().join("renamed.md")).unwrap();
            let after = file_ids_in_dir(dir.path());
            assert_eq!(after.get("renamed.md"), Some(&a));
            assert_eq!(after.get("b.md"), Some(&b));
            assert!(!after.contains_key("a.md"));
        }

        #[test]
        fn an_in_place_save_keeps_the_id() {
            let dir = tempfile::tempdir().unwrap();
            fs::write(dir.path().join("a.md"), "before").unwrap();
            let first = file_ids_in_dir(dir.path())["a.md"].clone();
            std::thread::sleep(std::time::Duration::from_millis(20));
            fs::write(dir.path().join("a.md"), "after, and longer").unwrap();
            assert_eq!(file_ids_in_dir(dir.path())["a.md"], first);
        }

        #[test]
        fn a_new_file_at_an_old_name_is_a_different_file() {
            let dir = tempfile::tempdir().unwrap();
            fs::write(dir.path().join("a.md"), "x").unwrap();
            let first = file_ids_in_dir(dir.path())["a.md"].clone();
            assert!(first.contains(':'), "the creation time is part of the id");
            fs::remove_file(dir.path().join("a.md")).unwrap();
            fs::write(dir.path().join("a.md"), "x").unwrap();
            assert_ne!(file_ids_in_dir(dir.path())["a.md"], first);
        }

        #[test]
        fn an_absent_directory_lists_nothing() {
            let dir = tempfile::tempdir().unwrap();
            assert!(file_ids_in_dir(&dir.path().join("gone")).is_empty());
        }
    }
}
