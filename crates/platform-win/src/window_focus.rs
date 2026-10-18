// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Foreground eligibility for a native window hosting child webviews.

/// Require the exact visible, restored top-level window to own foreground.
/// Keyboard focus may belong to a WebView child. An owned dialog is a separate
/// root and does not grant its owner eligibility.
#[allow(unsafe_code)]
pub fn is_foreground_root(window: isize) -> bool {
    #[cfg(windows)]
    {
        use windows::Win32::Foundation::HWND;
        use windows::Win32::UI::WindowsAndMessaging::{
            GetAncestor, GetForegroundWindow, IsIconic, IsWindowVisible, GA_ROOT,
        };

        if window == 0 {
            return false;
        }
        let expected = HWND(window as *mut std::ffi::c_void);
        // These APIs inspect opaque window handles without dereferencing caller memory.
        unsafe {
            GetAncestor(expected, GA_ROOT) == expected
                && IsWindowVisible(expected).as_bool()
                && !IsIconic(expected).as_bool()
                && GetAncestor(GetForegroundWindow(), GA_ROOT) == expected
        }
    }
    #[cfg(not(windows))]
    {
        let _ = window;
        false
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn absent_window_cannot_own_foreground() {
        assert!(!super::is_foreground_root(0));
    }
}
