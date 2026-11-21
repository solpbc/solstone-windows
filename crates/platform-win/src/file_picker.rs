// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! The native file dialog for import. A filter only narrows the first view;
//! the owner can always switch to every file.

#[cfg(windows)]
use observer_model::i18n::message;

use std::path::PathBuf;

/// A named file-type filter, for example `("*.zip, *.json", "*.zip;*.json")`.
pub struct Filter {
    pub name: String,
    pub pattern: String,
}

/// Show the dialog over `owner`. `Ok(None)` means the owner dismissed it.
#[cfg(windows)]
pub fn choose_files(
    owner: isize,
    filter: Option<&Filter>,
    multiple: bool,
) -> std::io::Result<Option<Vec<PathBuf>>> {
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
        let mut options = dialog.GetOptions().map_err(io_error)?
            | FOS_FILEMUSTEXIST
            | FOS_FORCEFILESYSTEM
            | FOS_NOCHANGEDIR;
        if multiple {
            options |= FOS_ALLOWMULTISELECT;
        }
        dialog.SetOptions(options).map_err(io_error)?;
        // The strings outlive SetFileTypes, which copies them.
        let mut names = Vec::new();
        let mut patterns = Vec::new();
        if let Some(filter) = filter {
            names.push(HSTRING::from(filter.name.as_str()));
            patterns.push(HSTRING::from(filter.pattern.as_str()));
        }
        names.push(HSTRING::from(message("windows.ui.files", &[])));
        patterns.push(HSTRING::from("*"));
        let specs: Vec<_> = names
            .iter()
            .zip(&patterns)
            .map(|(name, pattern)| COMDLG_FILTERSPEC {
                pszName: windows::core::PCWSTR(name.as_ptr()),
                pszSpec: windows::core::PCWSTR(pattern.as_ptr()),
            })
            .collect();
        dialog.SetFileTypes(&specs).map_err(io_error)?;
        if let Err(error) = dialog.Show(HWND(owner as *mut std::ffi::c_void)) {
            if error.code() == ERROR_CANCELLED.to_hresult() {
                return Ok(None);
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
        Ok(Some(paths))
    }
}

#[cfg(not(windows))]
pub fn choose_files(
    _owner: isize,
    _filter: Option<&Filter>,
    _multiple: bool,
) -> std::io::Result<Option<Vec<PathBuf>>> {
    Err(std::io::ErrorKind::Unsupported.into())
}
