// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! The native file dialog for import. A filter only narrows the first view;
//! the owner can always switch to every file.

#[cfg(windows)]
use observer_model::i18n::message;

use std::path::{Path, PathBuf};

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

/// Show a native Windows save dialog over `owner` with `suggested_filename`.
/// `Ok(None)` means the owner dismissed it.
#[cfg(windows)]
pub fn choose_save_file(
    owner: isize,
    suggested_filename: &str,
) -> std::io::Result<Option<PathBuf>> {
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
        FileSaveDialog, IFileSaveDialog, FOS_FORCEFILESYSTEM, FOS_NOCHANGEDIR, FOS_OVERWRITEPROMPT,
        FOS_PATHMUSTEXIST, SIGDN_FILESYSPATH,
    };
    let io_error = |error: Error| std::io::Error::other(format!("save dialog failed ({error})"));
    // SAFETY: all COM objects and returned strings belong to this thread's STA
    // apartment; every allocated path string is freed after copying.
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
        let dialog: IFileSaveDialog =
            CoCreateInstance(&FileSaveDialog, None, CLSCTX_INPROC_SERVER).map_err(io_error)?;
        let options = dialog.GetOptions().map_err(io_error)?
            | FOS_FORCEFILESYSTEM
            | FOS_NOCHANGEDIR
            | FOS_OVERWRITEPROMPT
            | FOS_PATHMUSTEXIST;
        dialog.SetOptions(options).map_err(io_error)?;
        let zip_name = HSTRING::from("*.zip");
        let zip_pat = HSTRING::from("*.zip");
        let all_name = HSTRING::from(message("windows.ui.files", &[]));
        let all_pat = HSTRING::from("*");
        let specs = [
            COMDLG_FILTERSPEC {
                pszName: windows::core::PCWSTR(zip_name.as_ptr()),
                pszSpec: windows::core::PCWSTR(zip_pat.as_ptr()),
            },
            COMDLG_FILTERSPEC {
                pszName: windows::core::PCWSTR(all_name.as_ptr()),
                pszSpec: windows::core::PCWSTR(all_pat.as_ptr()),
            },
        ];
        dialog.SetFileTypes(&specs).map_err(io_error)?;
        dialog.SetDefaultExtension(&HSTRING::from("zip")).ok();
        let suggested = if suggested_filename.is_empty() {
            "solstone-journal.zip"
        } else {
            suggested_filename
        };
        dialog.SetFileName(&HSTRING::from(suggested)).ok();
        if let Err(error) = dialog.Show(HWND(owner as *mut std::ffi::c_void)) {
            if error.code() == ERROR_CANCELLED.to_hresult() {
                return Ok(None);
            }
            return Err(io_error(error));
        }
        let item = dialog.GetResult().map_err(io_error)?;
        let name = item.GetDisplayName(SIGDN_FILESYSPATH).map_err(io_error)?;
        let path = PathBuf::from(OsString::from_wide(name.as_wide()));
        CoTaskMemFree(Some(name.0 as *const std::ffi::c_void));
        Ok(Some(path))
    }
}

#[cfg(not(windows))]
pub fn choose_save_file(
    _owner: isize,
    _suggested_filename: &str,
) -> std::io::Result<Option<PathBuf>> {
    Err(std::io::ErrorKind::Unsupported.into())
}

/// Available free bytes for the current user on the volume containing `dir`.
#[cfg(windows)]
pub fn available_disk_bytes(dir: &Path) -> Option<u64> {
    use windows::core::HSTRING;
    use windows::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;
    let wide = HSTRING::from(dir.as_os_str());
    let mut free_available = 0u64;
    // SAFETY: `wide` is NUL-terminated UTF-16 and `free_available` is a valid stack pointer.
    let ok = unsafe { GetDiskFreeSpaceExW(&wide, Some(&mut free_available), None, None) }.is_ok();
    ok.then_some(free_available)
}

#[cfg(not(windows))]
pub fn available_disk_bytes(_dir: &Path) -> Option<u64> {
    None
}

/// Default directory to probe for available device space before the owner picks a path.
pub fn default_space_probe_dir() -> PathBuf {
    if let Some(profile) = std::env::var_os("USERPROFILE") {
        let p = PathBuf::from(profile);
        let downloads = p.join("Downloads");
        if downloads.is_dir() {
            return downloads;
        }
        if p.is_dir() {
            return p;
        }
    }
    if let Some(home) = std::env::var_os("HOME") {
        let p = PathBuf::from(home);
        if p.is_dir() {
            return p;
        }
    }
    std::env::temp_dir()
}

/// Atomically move `from` over `to`, replacing an existing destination file.
#[cfg(windows)]
pub fn replace_file_atomic(from: &Path, to: &Path) -> std::io::Result<()> {
    use windows::core::HSTRING;
    use windows::Win32::Storage::FileSystem::{
        MoveFileExW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
    };
    let src = HSTRING::from(from.as_os_str());
    let dst = HSTRING::from(to.as_os_str());
    // SAFETY: both HSTRING buffers stay alive across the synchronous Win32 call.
    unsafe {
        MoveFileExW(
            &src,
            &dst,
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    }
    .map_err(|err| std::io::Error::from_raw_os_error(err.code().0))
}

#[cfg(not(windows))]
pub fn replace_file_atomic(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::rename(from, to)
}

/// Open File Explorer and select `path`.
#[cfg(windows)]
pub fn reveal_in_explorer(path: &Path) -> bool {
    use windows::core::HSTRING;
    use windows::Win32::System::Com::{
        CoInitializeEx, CoUninitialize, COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE,
    };
    use windows::Win32::UI::Shell::{ILCreateFromPathW, ILFree, SHOpenFolderAndSelectItems};
    // SAFETY: COM apartment is scoped to this call and the allocated PIDL is always freed.
    unsafe {
        let init = CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE);
        if init.is_err() {
            return false;
        }
        struct Guard;
        impl Drop for Guard {
            fn drop(&mut self) {
                unsafe { CoUninitialize() };
            }
        }
        let _guard = Guard;
        let wide = HSTRING::from(path.as_os_str());
        let pidl = ILCreateFromPathW(&wide);
        if pidl.is_null() {
            return false;
        }
        let ok = SHOpenFolderAndSelectItems(pidl, None, 0).is_ok();
        ILFree(Some(pidl));
        ok
    }
}

#[cfg(not(windows))]
pub fn reveal_in_explorer(_path: &Path) -> bool {
    false
}
