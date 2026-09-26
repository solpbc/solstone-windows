// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! The main-thread stack reserve a PE executable asks Windows for.
//!
//! Tauri dispatches IPC on the app's main thread, so every async command's future
//! is built and moved there. At the MSVC default 1 MiB reserve, pressing pair in
//! 2.0.11 overflowed that thread and closed the app. `src-tauri/build.rs` links the
//! app with `/STACK:8388608`, and the release finalizer refuses an executable that
//! reserves less, so the link argument cannot quietly disappear.

/// The smallest main-thread stack reserve a release executable may carry.
pub const MAIN_THREAD_STACK_RESERVE: u64 = 8 * 1024 * 1024;

const PE32_MAGIC: u16 = 0x10b;
const PE32_PLUS_MAGIC: u16 = 0x20b;

/// `SizeOfStackReserve` from the optional header, or `None` when the bytes are not
/// a well-formed PE32 or PE32+ image.
pub fn stack_reserve(image: &[u8]) -> Option<u64> {
    if image.get(..2)? != b"MZ" {
        return None;
    }
    let pe = usize::try_from(read_u32(image, 0x3c)?).ok()?;
    if image.get(pe..pe.checked_add(4)?)? != b"PE\0\0" {
        return None;
    }
    let optional = pe.checked_add(24)?;
    match read_u16(image, optional)? {
        PE32_MAGIC => read_u32(image, optional.checked_add(72)?).map(u64::from),
        PE32_PLUS_MAGIC => read_u64(image, optional.checked_add(72)?),
        _ => None,
    }
}

fn read_u16(bytes: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(
        bytes.get(at..at.checked_add(2)?)?.try_into().ok()?,
    ))
}

fn read_u32(bytes: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(
        bytes.get(at..at.checked_add(4)?)?.try_into().ok()?,
    ))
}

fn read_u64(bytes: &[u8], at: usize) -> Option<u64> {
    Some(u64::from_le_bytes(
        bytes.get(at..at.checked_add(8)?)?.try_into().ok()?,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn image(magic: u16, reserve: u64) -> Vec<u8> {
        let pe = 0x80;
        let mut bytes = vec![0_u8; 0x200];
        bytes[..2].copy_from_slice(b"MZ");
        bytes[0x3c..0x40].copy_from_slice(&(pe as u32).to_le_bytes());
        bytes[pe..pe + 4].copy_from_slice(b"PE\0\0");
        let optional = pe + 24;
        bytes[optional..optional + 2].copy_from_slice(&magic.to_le_bytes());
        if magic == PE32_PLUS_MAGIC {
            bytes[optional + 72..optional + 80].copy_from_slice(&reserve.to_le_bytes());
        } else {
            bytes[optional + 72..optional + 76].copy_from_slice(&(reserve as u32).to_le_bytes());
        }
        bytes
    }

    #[test]
    fn reads_the_reserve_from_pe32_plus_and_pe32() {
        assert_eq!(
            stack_reserve(&image(PE32_PLUS_MAGIC, MAIN_THREAD_STACK_RESERVE)),
            Some(MAIN_THREAD_STACK_RESERVE)
        );
        assert_eq!(
            stack_reserve(&image(PE32_MAGIC, 1024 * 1024)),
            Some(1024 * 1024)
        );
    }

    #[test]
    fn refuses_what_is_not_a_pe_image() {
        assert_eq!(stack_reserve(b""), None);
        assert_eq!(stack_reserve(b"MZ"), None);
        let mut not_pe = image(PE32_PLUS_MAGIC, MAIN_THREAD_STACK_RESERVE);
        not_pe[0x80] = b'X';
        assert_eq!(stack_reserve(&not_pe), None);
        assert_eq!(
            stack_reserve(&image(0x107, MAIN_THREAD_STACK_RESERVE)),
            None
        );
        let mut far = image(PE32_PLUS_MAGIC, MAIN_THREAD_STACK_RESERVE);
        far[0x3c..0x40].copy_from_slice(&u32::MAX.to_le_bytes());
        assert_eq!(stack_reserve(&far), None);
    }
}
