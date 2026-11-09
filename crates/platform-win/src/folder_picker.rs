// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! The native "choose a folder" dialog.

use std::path::PathBuf;

/// Show the folder dialog, owned by a window. `Ok(None)` when the owner cancels.
#[cfg(windows)]
pub fn choose_folder(owner: isize, title: &str) -> std::io::Result<Option<PathBuf>> {
    use std::ffi::OsString;
    use std::os::windows::ffi::OsStringExt;

    use windows::core::{Error, HSTRING, PWSTR};
    use windows::Win32::Foundation::{ERROR_CANCELLED, HWND};
    use windows::Win32::System::Com::{
        CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize, CLSCTX_INPROC_SERVER,
        COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE,
    };
    use windows::Win32::UI::Shell::{
        FileOpenDialog, IFileOpenDialog, FOS_FORCEFILESYSTEM, FOS_NOCHANGEDIR, FOS_PICKFOLDERS,
        SIGDN_FILESYSPATH,
    };

    let io_error = |error: Error| std::io::Error::other(format!("folder dialog failed ({error})"));
    // SAFETY: COM is initialised for this thread for the duration of the dialog
    // and uninitialised on the same thread when `Guard` drops. Every interface
    // pointer and string below is owned by this call; `GetDisplayName` returns a
    // CoTaskMem string that is freed once read.
    unsafe {
        let init = CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE);
        struct Guard(bool);
        impl Drop for Guard {
            fn drop(&mut self) {
                if self.0 {
                    unsafe { CoUninitialize() };
                }
            }
        }
        let _guard = Guard(init.is_ok());

        let dialog: IFileOpenDialog =
            CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER).map_err(io_error)?;
        let options = dialog.GetOptions().map_err(io_error)?;
        dialog
            .SetOptions(options | FOS_PICKFOLDERS | FOS_FORCEFILESYSTEM | FOS_NOCHANGEDIR)
            .map_err(io_error)?;
        dialog.SetTitle(&HSTRING::from(title)).map_err(io_error)?;
        let owner = HWND(owner as *mut std::ffi::c_void);
        if let Err(error) = dialog.Show(owner) {
            if error.code() == ERROR_CANCELLED.to_hresult() {
                return Ok(None);
            }
            return Err(io_error(error));
        }
        let item = dialog.GetResult().map_err(io_error)?;
        let name: PWSTR = item.GetDisplayName(SIGDN_FILESYSPATH).map_err(io_error)?;
        let path = name.as_wide();
        let path = PathBuf::from(OsString::from_wide(path));
        CoTaskMemFree(Some(name.0 as *const std::ffi::c_void));
        Ok(Some(path))
    }
}

#[cfg(not(windows))]
pub fn choose_folder(_owner: isize, _title: &str) -> std::io::Result<Option<PathBuf>> {
    Err(std::io::ErrorKind::Unsupported.into())
}
