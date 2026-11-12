// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! The native multiple-file dialog for import.

use std::path::PathBuf;

#[cfg(windows)]
pub fn choose_files(owner: isize) -> std::io::Result<Vec<PathBuf>> {
    use std::ffi::OsString;
    use std::os::windows::ffi::OsStringExt;
    use windows::core::{Error, HSTRING};
    use windows::Win32::Foundation::{ERROR_CANCELLED, HWND};
    use windows::Win32::System::Com::{
        CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize, CLSCTX_INPROC_SERVER,
        COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE,
    };
    use windows::Win32::UI::Shell::Common::COMDLG_FILTERSPEC;
    use windows::Win32::UI::Shell::{
        FileOpenDialog, IFileOpenDialog, FOS_ALLOWMULTISELECT, FOS_FILEMUSTEXIST,
        FOS_FORCEFILESYSTEM, FOS_NOCHANGEDIR, SIGDN_FILESYSPATH,
    };
    let io_error = |error: Error| std::io::Error::other(format!("file dialog failed ({error})"));
    // SAFETY: all COM objects and returned strings belong to this thread and
    // remain scoped within its apartment. Every allocated path is freed after
    // copying; no native pointer leaves this call.
    unsafe {
        let init = CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE);
        if init.is_err() {
            return Err(io_error(Error::from_hresult(init)));
        }
        struct Guard;
        impl Drop for Guard {
            fn drop(&mut self) {
                unsafe { CoUninitialize() };
            }
        }
        let _guard = Guard;
        let dialog: IFileOpenDialog =
            CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER).map_err(io_error)?;
        dialog
            .SetOptions(
                dialog.GetOptions().map_err(io_error)?
                    | FOS_ALLOWMULTISELECT
                    | FOS_FILEMUSTEXIST
                    | FOS_FORCEFILESYSTEM
                    | FOS_NOCHANGEDIR,
            )
            .map_err(io_error)?;
        let label = HSTRING::from("files");
        let pattern = HSTRING::from("*.pdf;*.txt;*.md;*.markdown;*.log;*.png;*.jpg;*.jpeg;*.gif;*.webp;*.mp3;*.m4a;*.aac;*.ogg;*.oga;*.opus;*.flac;*.wav;*.amr;*.aif;*.aiff");
        dialog
            .SetFileTypes(&[COMDLG_FILTERSPEC {
                pszName: windows::core::PCWSTR(label.as_ptr()),
                pszSpec: windows::core::PCWSTR(pattern.as_ptr()),
            }])
            .map_err(io_error)?;
        if let Err(error) = dialog.Show(HWND(owner as *mut std::ffi::c_void)) {
            if error.code() == ERROR_CANCELLED.to_hresult() {
                return Ok(Vec::new());
            }
            return Err(io_error(error));
        }
        let items = dialog.GetResults().map_err(io_error)?;
        let mut paths = Vec::new();
        for index in 0..items.GetCount().map_err(io_error)? {
            let item = items.GetItemAt(index).map_err(io_error)?;
            let name = item.GetDisplayName(SIGDN_FILESYSPATH).map_err(io_error)?;
            let path = PathBuf::from(OsString::from_wide(name.as_wide()));
            CoTaskMemFree(Some(name.0 as *const std::ffi::c_void));
            paths.push(path);
        }
        Ok(paths)
    }
}

#[cfg(not(windows))]
pub fn choose_files(_owner: isize) -> std::io::Result<Vec<PathBuf>> {
    Err(std::io::ErrorKind::Unsupported.into())
}
