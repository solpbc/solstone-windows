// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! The computer, for the screenshots source: Windows' own Screenshots folder,
//! a listing that never opens a file (so a OneDrive file that is only in the
//! cloud is never downloaded to count it), and WIC conversion to PNG for an
//! image type the journal doesn't take.

use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use observer_model::LocalOffset;
use screenshots_source::{ConvertError, ListError, Listed, Platform};

const FILE_ATTRIBUTE_HIDDEN: u32 = 0x2;
const FILE_ATTRIBUTE_SYSTEM: u32 = 0x4;
const FILE_ATTRIBUTE_OFFLINE: u32 = 0x1000;
const FILE_ATTRIBUTE_RECALL_ON_OPEN: u32 = 0x4_0000;
const FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS: u32 = 0x40_0000;

/// The screenshots source's view of this computer.
#[derive(Debug, Default)]
pub struct WindowsScreenshots;

impl Platform for WindowsScreenshots {
    fn now_ms(&self) -> i64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
            .unwrap_or(0)
    }

    fn offset_at(&self, secs: i64) -> Option<i32> {
        let secs = u64::try_from(secs).ok()?;
        let zone = crate::WindowsLocalOffset.local_zone(secs).ok()?;
        i32::try_from(zone.utc_offset_seconds).ok()
    }

    fn own_folder(&self) -> Option<PathBuf> {
        own_folder()
    }

    fn home(&self) -> Option<PathBuf> {
        std::env::var_os("USERPROFILE").map(PathBuf::from)
    }

    fn list(&self, dir: &Path) -> Result<Vec<Listed>, ListError> {
        list_folder(dir)
    }

    fn read(&self, path: &Path) -> io::Result<Vec<u8>> {
        std::fs::read(path)
    }

    fn to_png(&self, bytes: &[u8]) -> Result<Vec<u8>, ConvertError> {
        to_png(bytes)
    }
}

/// The top level of a folder. Each entry's facts come from the directory
/// listing itself, so no file is opened.
pub fn list_folder(dir: &Path) -> Result<Vec<Listed>, ListError> {
    let entries = std::fs::read_dir(dir).map_err(|error| match error.kind() {
        io::ErrorKind::NotFound => ListError::Missing,
        _ => ListError::Unreadable,
    })?;
    let mut listed = Vec::new();
    for entry in entries {
        let Ok(entry) = entry else { continue };
        // On Windows this is the enumeration's own record, not an open.
        let Ok(meta) = entry.metadata() else { continue };
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        let attributes = attributes(&meta);
        listed.push(Listed {
            path: entry.path(),
            name,
            size: meta.len(),
            modified_ms: meta.modified().map(millis).unwrap_or(0),
            created_ms: meta.created().ok().map(millis),
            regular: meta.file_type().is_file(),
            hidden: attributes & FILE_ATTRIBUTE_HIDDEN != 0,
            system: attributes & FILE_ATTRIBUTE_SYSTEM != 0,
            cloud_only: attributes
                & (FILE_ATTRIBUTE_OFFLINE
                    | FILE_ATTRIBUTE_RECALL_ON_OPEN
                    | FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS)
                != 0,
        });
    }
    Ok(listed)
}

fn millis(at: SystemTime) -> i64 {
    at.duration_since(UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

#[cfg(windows)]
fn attributes(meta: &std::fs::Metadata) -> u32 {
    use std::os::windows::fs::MetadataExt;
    meta.file_attributes()
}

#[cfg(not(windows))]
fn attributes(_meta: &std::fs::Metadata) -> u32 {
    0
}

/// Windows' own Screenshots folder, resolved without checking it exists: a
/// PC that has never saved a screenshot has the path and no folder yet.
#[cfg(windows)]
pub fn own_folder() -> Option<PathBuf> {
    use std::ffi::OsString;
    use std::os::windows::ffi::OsStringExt;

    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::System::Com::CoTaskMemFree;
    use windows::Win32::UI::Shell::{
        FOLDERID_Screenshots, SHGetKnownFolderPath, KF_FLAG_DONT_VERIFY,
    };

    // SAFETY: the GUID is a static constant; the returned string is a
    // CoTaskMem allocation owned by this call, copied out and then freed.
    unsafe {
        let path = SHGetKnownFolderPath(
            &FOLDERID_Screenshots,
            KF_FLAG_DONT_VERIFY,
            HANDLE::default(),
        )
        .ok()?;
        let resolved = PathBuf::from(OsString::from_wide(path.as_wide()));
        CoTaskMemFree(Some(path.0 as *const std::ffi::c_void));
        (!resolved.as_os_str().is_empty()).then_some(resolved)
    }
}

#[cfg(not(windows))]
pub fn own_folder() -> Option<PathBuf> {
    None
}

/// Convert one image to PNG through WIC: the first frame, at its own size and
/// resolution, with its ICC colour profile, and the EXIF and XMP blocks the
/// system's PNG encoder takes. A type with no decoder here is undecodable.
#[cfg(windows)]
pub fn to_png(bytes: &[u8]) -> Result<Vec<u8>, ConvertError> {
    use windows::core::{Interface, GUID, PROPVARIANT};
    use windows::Win32::Foundation::WINCODEC_ERR_COMPONENTNOTFOUND;
    use windows::Win32::Graphics::Imaging::{
        CLSID_WICImagingFactory, GUID_ContainerFormatPng, GUID_WICPixelFormat1bppIndexed,
        GUID_WICPixelFormat2bppIndexed, GUID_WICPixelFormat32bppBGRA,
        GUID_WICPixelFormat4bppIndexed, GUID_WICPixelFormat8bppIndexed, IWICBitmapFrameEncode,
        IWICBitmapSource, IWICColorContext, IWICImagingFactory, WICBitmapDitherTypeNone,
        WICBitmapEncoderNoCache, WICBitmapPaletteTypeCustom, WICColorContextProfile,
        WICDecodeMetadataCacheOnDemand,
    };
    use windows::Win32::System::Com::StructuredStorage::{CreateStreamOnHGlobal, IPropertyBag2};
    use windows::Win32::System::Com::{
        CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_INPROC_SERVER,
        COINIT_MULTITHREADED, STREAM_SEEK_END, STREAM_SEEK_SET,
    };

    let unavailable = |_| ConvertError::Unavailable;
    // SAFETY: COM is initialised on this thread for the conversion and released
    // when `Guard` drops. Every interface is owned here; buffers passed to WIC
    // outlive the calls that read them.
    unsafe {
        let init = CoInitializeEx(None, COINIT_MULTITHREADED);
        struct Guard(bool);
        impl Drop for Guard {
            fn drop(&mut self) {
                if self.0 {
                    unsafe { CoUninitialize() };
                }
            }
        }
        let _guard = Guard(init.is_ok());

        let factory: IWICImagingFactory =
            CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER)
                .map_err(unavailable)?;
        let input = factory.CreateStream().map_err(unavailable)?;
        input.InitializeFromMemory(bytes).map_err(unavailable)?;
        let decoder = factory
            .CreateDecoderFromStream(&input, std::ptr::null(), WICDecodeMetadataCacheOnDemand)
            .map_err(|_| ConvertError::Undecodable)?;
        let frame = decoder.GetFrame(0).map_err(|_| ConvertError::Undecodable)?;

        let output = CreateStreamOnHGlobal(None, true).map_err(unavailable)?;
        let encoder = factory
            .CreateEncoder(&GUID_ContainerFormatPng, std::ptr::null())
            .map_err(unavailable)?;
        encoder
            .Initialize(&output, WICBitmapEncoderNoCache)
            .map_err(unavailable)?;
        let mut target: Option<IWICBitmapFrameEncode> = None;
        let mut options: Option<IPropertyBag2> = None;
        encoder
            .CreateNewFrame(&mut target, &mut options)
            .map_err(unavailable)?;
        let target = target.ok_or(ConvertError::Unavailable)?;
        target.Initialize(options.as_ref()).map_err(unavailable)?;

        let (mut width, mut height) = (0u32, 0u32);
        frame
            .GetSize(&mut width, &mut height)
            .map_err(|_| ConvertError::Undecodable)?;
        target.SetSize(width, height).map_err(unavailable)?;
        let (mut dpi_x, mut dpi_y) = (96.0f64, 96.0f64);
        if frame.GetResolution(&mut dpi_x, &mut dpi_y).is_ok() {
            let _ = target.SetResolution(dpi_x, dpi_y);
        }

        // The colour profile, so the picture looks as it did.
        let mut count = 0u32;
        if frame.GetColorContexts(&mut [], &mut count).is_ok() && count > 0 {
            let mut contexts: Vec<Option<IWICColorContext>> = (0..count)
                .map(|_| factory.CreateColorContext().ok())
                .collect();
            if contexts.iter().all(Option::is_some)
                && frame.GetColorContexts(&mut contexts, &mut count).is_ok()
            {
                let profiles: Vec<Option<IWICColorContext>> = contexts
                    .into_iter()
                    .flatten()
                    .filter(|c| c.GetType().is_ok_and(|t| t == WICColorContextProfile))
                    .take(1)
                    .map(Some)
                    .collect();
                if !profiles.is_empty() {
                    let _ = target.SetColorContexts(&profiles);
                }
            }
        }

        // EXIF and XMP, block by block, where the PNG encoder takes them.
        if let (Ok(reader), Ok(writer)) = (
            frame.GetMetadataQueryReader(),
            target.GetMetadataQueryWriter(),
        ) {
            for (from, to) in [
                ("/xmp", "/xmp"),
                ("/ifd/xmp", "/xmp"),
                ("/ifd", "/ifd"),
                ("/ifd/exif", "/ifd/exif"),
            ] {
                let mut value = PROPVARIANT::default();
                if reader
                    .GetMetadataByName(&windows::core::HSTRING::from(from), &mut value)
                    .is_err()
                {
                    continue;
                }
                let Ok(unknown) = windows::core::IUnknown::try_from(&value) else {
                    continue;
                };
                let Ok(block) =
                    unknown.cast::<windows::Win32::Graphics::Imaging::IWICMetadataQueryReader>()
                else {
                    continue;
                };
                let Ok(copy) = factory.CreateQueryWriterFromReader(&block, std::ptr::null()) else {
                    continue;
                };
                let Ok(copy) = copy.cast::<windows::core::IUnknown>() else {
                    continue;
                };
                let value = PROPVARIANT::from(copy);
                let _ = writer.SetMetadataByName(&windows::core::HSTRING::from(to), &value);
            }
        }

        let source_format: GUID = frame
            .GetPixelFormat()
            .map_err(|_| ConvertError::Undecodable)?;
        let indexed = [
            GUID_WICPixelFormat1bppIndexed,
            GUID_WICPixelFormat2bppIndexed,
            GUID_WICPixelFormat4bppIndexed,
            GUID_WICPixelFormat8bppIndexed,
        ]
        .contains(&source_format);
        let mut format = if indexed {
            GUID_WICPixelFormat32bppBGRA
        } else {
            source_format
        };
        target.SetPixelFormat(&mut format).map_err(unavailable)?;
        let source: IWICBitmapSource = if format == source_format {
            frame.cast().map_err(unavailable)?
        } else {
            let converter = factory.CreateFormatConverter().map_err(unavailable)?;
            converter
                .Initialize(
                    &frame,
                    &format,
                    WICBitmapDitherTypeNone,
                    None,
                    0.0,
                    WICBitmapPaletteTypeCustom,
                )
                .map_err(|_| ConvertError::Undecodable)?;
            converter.cast().map_err(unavailable)?
        };
        target
            .WriteSource(&source, std::ptr::null())
            .map_err(|error| {
                if error.code() == WINCODEC_ERR_COMPONENTNOTFOUND {
                    ConvertError::Undecodable
                } else {
                    ConvertError::Unavailable
                }
            })?;
        target.Commit().map_err(unavailable)?;
        encoder.Commit().map_err(unavailable)?;

        let mut size = 0u64;
        output
            .Seek(0, STREAM_SEEK_END, Some(&mut size))
            .map_err(unavailable)?;
        output.Seek(0, STREAM_SEEK_SET, None).map_err(unavailable)?;
        let size = usize::try_from(size).map_err(|_| ConvertError::Unavailable)?;
        let mut png = vec![0u8; size];
        let mut filled = 0usize;
        while filled < size {
            let mut read = 0u32;
            let chunk = u32::try_from(size - filled).unwrap_or(u32::MAX);
            output
                .Read(png[filled..].as_mut_ptr().cast(), chunk, Some(&mut read))
                .ok()
                .map_err(unavailable)?;
            if read == 0 {
                return Err(ConvertError::Unavailable);
            }
            filled += read as usize;
        }
        Ok(png)
    }
}

#[cfg(not(windows))]
pub fn to_png(_bytes: &[u8]) -> Result<Vec<u8>, ConvertError> {
    Err(ConvertError::Unavailable)
}
