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

/// A 2 by 1 RGB TIFF whose IFD0 Make is `make`.
fn tiff_with_make(make: &str) -> Vec<u8> {
    let le16 = |v: u16| v.to_le_bytes();
    let le32 = |v: u32| v.to_le_bytes();
    let make = format!("{make}\0");
    let entries: u16 = 9;
    let make_at = 8 + 2 + u32::from(entries) * 12 + 4;
    let bits_at = make_at + make.len() as u32 + (make.len() as u32 % 2);
    let pixels_at = bits_at + 6;
    let mut out = b"II*\0".to_vec();
    out.extend_from_slice(&le32(8));
    out.extend_from_slice(&le16(entries));
    for (tag, kind, count, value) in [
        (256u16, 4u16, 1u32, 2u32),
        (257, 4, 1, 1),
        (258, 3, 3, bits_at),
        (259, 3, 1, 1),
        (262, 3, 1, 2),
        (271, 2, make.len() as u32, make_at),
        (273, 4, 1, pixels_at),
        (277, 3, 1, 3),
        (279, 4, 1, 6),
    ] {
        out.extend_from_slice(&le16(tag));
        out.extend_from_slice(&le16(kind));
        out.extend_from_slice(&le32(count));
        out.extend_from_slice(&le32(value));
    }
    out.extend_from_slice(&le32(0));
    out.extend_from_slice(make.as_bytes());
    while out.len() < bits_at as usize {
        out.push(0);
    }
    for _ in 0..3 {
        out.extend_from_slice(&le16(8));
    }
    out.extend_from_slice(&[255, 0, 0, 0, 0, 255]);
    out
}

/// Each chunk's type and data, in order.
fn chunks(png: &[u8]) -> Vec<([u8; 4], Vec<u8>)> {
    let mut out = Vec::new();
    let mut at = 8;
    while at + 8 <= png.len() {
        let len = u32::from_be_bytes(png[at..at + 4].try_into().unwrap()) as usize;
        let kind: [u8; 4] = png[at + 4..at + 8].try_into().unwrap();
        out.push((kind, png[at + 8..at + 8 + len].to_vec()));
        at += 12 + len;
    }
    out
}

#[test]
fn a_tiff_keeps_its_exif_make_in_an_exif_chunk_before_the_pixels() {
    let png = to_png(&tiff_with_make("fixturemake")).unwrap();
    let list = chunks(&png);
    let exif = list
        .iter()
        .position(|(kind, _)| kind == b"eXIf")
        .expect("an eXIf chunk");
    let idat = list.iter().position(|(kind, _)| kind == b"IDAT").unwrap();
    assert!(exif < idat, "eXIf comes before the first IDAT");
    let data = &list[exif].1;
    assert!(data.starts_with(b"II*\0"), "a TIFF header, no Exif prefix");
    assert!(data.windows(11).any(|w| w == b"fixturemake"));
}
