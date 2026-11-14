// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Native notices that must remain visible when no webview can open.

#[cfg(windows)]
pub fn show(title: &str, body: &str) {
    use windows::{
        core::PCWSTR,
        Win32::UI::WindowsAndMessaging::{
            MessageBoxW, MB_ICONINFORMATION, MB_OK, MB_SETFOREGROUND,
        },
    };
    let title: Vec<u16> = title.encode_utf16().chain(Some(0)).collect();
    let body: Vec<u16> = body.encode_utf16().chain(Some(0)).collect();
    // Both nul-terminated buffers live through the synchronous dialog call.
    unsafe {
        MessageBoxW(
            None,
            PCWSTR(body.as_ptr()),
            PCWSTR(title.as_ptr()),
            MB_OK | MB_ICONINFORMATION | MB_SETFOREGROUND,
        );
    }
}

#[cfg(not(windows))]
pub fn show(_title: &str, _body: &str) {}
