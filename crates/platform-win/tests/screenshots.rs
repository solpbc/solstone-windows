// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! The screenshots source's Windows seams, on a real Windows file system and
//! WIC. Windows-only: the host build has neither.

#![cfg(windows)]

use platform_win::screenshots::{list_folder, own_folder, to_png};
use screenshots_source::{ConvertError, ListError};

/// A 2 by 1, 24-bit BMP: one red pixel, one blue.
fn tiny_bmp() -> Vec<u8> {
    let pixels: [u8; 8] = [0, 0, 255, 255, 0, 0, 0, 0];
    let mut out = Vec::new();
    out.extend_from_slice(b"BM");
    out.extend_from_slice(&(54u32 + 8).to_le_bytes());
    out.extend_from_slice(&[0; 4]);
    out.extend_from_slice(&54u32.to_le_bytes());
    out.extend_from_slice(&40u32.to_le_bytes());
    out.extend_from_slice(&2i32.to_le_bytes());
    out.extend_from_slice(&1i32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&24u16.to_le_bytes());
    out.extend_from_slice(&[0; 4]);
    out.extend_from_slice(&8u32.to_le_bytes());
    out.extend_from_slice(&[0; 16]);
    out.extend_from_slice(&pixels);
    out
}

#[test]
fn windows_names_its_screenshots_folder_whether_or_not_it_exists() {
    let folder = own_folder().expect("a known folder path");
    assert!(folder.is_absolute(), "{}", folder.display());
}

#[test]
fn the_listing_reads_the_top_level_without_opening_anything() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("Screenshot (1).png"), b"png").unwrap();
    std::fs::create_dir(dir.path().join("inner")).unwrap();
    std::fs::write(dir.path().join("inner").join("deep.png"), b"png").unwrap();
    let listed = list_folder(dir.path()).unwrap();
    assert_eq!(
        listed.len(),
        2,
        "the file and the folder, not what's inside it"
    );
    let file = listed
        .iter()
        .find(|e| e.name == "Screenshot (1).png")
        .unwrap();
    assert!(file.regular && !file.hidden && !file.system && !file.cloud_only);
    assert_eq!(file.size, 3);
    assert!(file.created_ms.is_some(), "NTFS keeps a creation time");
    assert!(!listed.iter().find(|e| e.name == "inner").unwrap().regular);
    assert_eq!(
        list_folder(&dir.path().join("gone")).unwrap_err(),
        ListError::Missing
    );
}

#[test]
fn a_bmp_becomes_a_png_of_the_same_size() {
    let png = to_png(&tiny_bmp()).unwrap();
    assert!(png.starts_with(b"\x89PNG\r\n\x1a\n"));
    assert_eq!(&png[12..16], b"IHDR");
    assert_eq!(u32::from_be_bytes(png[16..20].try_into().unwrap()), 2);
    assert_eq!(u32::from_be_bytes(png[20..24].try_into().unwrap()), 1);
    assert_eq!(
        to_png(&tiny_bmp()).unwrap(),
        png,
        "the same bytes every time"
    );
}

#[test]
fn garbage_is_undecodable() {
    assert_eq!(
        to_png(b"not an image at all").unwrap_err(),
        ConvertError::Undecodable
    );
}
