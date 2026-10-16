// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Native destination selection for a separately reviewed log snapshot.

use std::path::PathBuf;

#[cfg(windows)]
pub fn choose_destination(owner: isize) -> std::io::Result<Option<PathBuf>> {
    use std::ffi::OsString;
    use std::os::windows::ffi::OsStringExt;
    use windows::core::{w, PWSTR};
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::Controls::Dialogs::{
        CommDlgExtendedError, GetSaveFileNameW, OFN_DONTADDTORECENT, OFN_EXPLORER, OFN_NOCHANGEDIR,
        OFN_OVERWRITEPROMPT, OFN_PATHMUSTEXIST, OPENFILENAMEW,
    };

    let mut filename = vec![0u16; 32768];
    let initial: Vec<u16> = "solstone-logs.txt".encode_utf16().collect();
    filename[..initial.len()].copy_from_slice(&initial);
    let mut dialog = OPENFILENAMEW {
        lStructSize: std::mem::size_of::<OPENFILENAMEW>() as u32,
        hwndOwner: HWND(owner as *mut std::ffi::c_void),
        lpstrFile: PWSTR(filename.as_mut_ptr()),
        nMaxFile: filename.len() as u32,
        lpstrFilter: w!("text files (*.txt)\0*.txt\0\0"),
        lpstrDefExt: w!("txt"),
        lpstrTitle: w!("save a copy of recent logs"),
        Flags: OFN_EXPLORER
            | OFN_NOCHANGEDIR
            | OFN_PATHMUSTEXIST
            | OFN_OVERWRITEPROMPT
            | OFN_DONTADDTORECENT,
        ..Default::default()
    };
    // SAFETY: the owner handle comes from the live Settings window. All UTF-16
    // buffers remain alive across the synchronous dialog, and nMaxFile is the
    // actual writable capacity. The API writes within that buffer.
    if unsafe { GetSaveFileNameW(&mut dialog) }.as_bool() {
        let length = filename
            .iter()
            .position(|c| *c == 0)
            .unwrap_or(filename.len());
        Ok(Some(PathBuf::from(OsString::from_wide(
            &filename[..length],
        ))))
    } else {
        // SAFETY: this thread just returned from the common dialog.
        let code = unsafe { CommDlgExtendedError() }.0;
        if code == 0 {
            Ok(None)
        } else {
            Err(std::io::Error::other(format!(
                "save dialog failed ({code})"
            )))
        }
    }
}

#[cfg(not(windows))]
pub fn choose_destination(_owner: isize) -> std::io::Result<Option<PathBuf>> {
    Err(std::io::ErrorKind::Unsupported.into())
}
