// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Native caption for a window whose page paints the sun-arc ground.
//!
//! Windows 11 accepts a caption and border color. Windows 10 refuses those
//! attributes; the caller ignores the failure and the system caption stays.
//! The buttons stay the ones Windows draws. Caption text and the icon are
//! hidden. The window title itself is left alone, for the taskbar and for
//! accessibility.

/// Pack a color as COLORREF, `0x00BBGGRR`.
pub fn colorref(red: u8, green: u8, blue: u8) -> u32 {
    u32::from(blue) << 16 | u32::from(green) << 8 | u32::from(red)
}

/// A readable caption-text color, used only if Windows still draws the title.
pub fn caption_text_color(red: u8, green: u8, blue: u8) -> u32 {
    let luminance = 0.2126 * f32::from(red) + 0.7152 * f32::from(green) + 0.0722 * f32::from(blue);
    if luminance > 140.0 {
        colorref(0x1A, 0x1A, 0x1A)
    } else {
        colorref(0xFC, 0xF3, 0xE4)
    }
}

/// Paint the native caption and hide its text and icon.
///
/// Returns false when the handle is absent or the OS refuses an attribute.
#[allow(unsafe_code)]
pub fn apply_caption_colors(window: isize, red: u8, green: u8, blue: u8) -> bool {
    #[cfg(windows)]
    {
        use windows::Win32::Foundation::HWND;
        use windows::Win32::Graphics::Dwm::{
            DwmSetWindowAttribute, DWMWA_BORDER_COLOR, DWMWA_CAPTION_COLOR, DWMWA_TEXT_COLOR,
        };
        use windows::Win32::UI::Controls::{
            SetWindowThemeAttribute, WTA_NONCLIENT, WTA_OPTIONS, WTNCA_NODRAWCAPTION,
            WTNCA_NODRAWICON,
        };

        if window == 0 {
            return false;
        }
        let caption = colorref(red, green, blue);
        let text = caption_text_color(red, green, blue);
        let border = caption;
        let hwnd = HWND(window as *mut std::ffi::c_void);
        // Color first, then hide the caption text. Each call passes a local
        // value by pointer and does not retain it. A refusal leaves that
        // attribute as the system drew it.
        unsafe {
            let border_ok = DwmSetWindowAttribute(
                hwnd,
                DWMWA_BORDER_COLOR,
                &border as *const u32 as *const _,
                std::mem::size_of::<u32>() as u32,
            )
            .is_ok();
            let caption_ok = DwmSetWindowAttribute(
                hwnd,
                DWMWA_CAPTION_COLOR,
                &caption as *const u32 as *const _,
                std::mem::size_of::<u32>() as u32,
            )
            .is_ok();
            let text_ok = DwmSetWindowAttribute(
                hwnd,
                DWMWA_TEXT_COLOR,
                &text as *const u32 as *const _,
                std::mem::size_of::<u32>() as u32,
            )
            .is_ok();
            let options = WTA_OPTIONS {
                dwFlags: WTNCA_NODRAWCAPTION | WTNCA_NODRAWICON,
                dwMask: WTNCA_NODRAWCAPTION | WTNCA_NODRAWICON,
            };
            let hidden_ok = SetWindowThemeAttribute(
                hwnd,
                WTA_NONCLIENT,
                &options as *const WTA_OPTIONS as *const _,
                std::mem::size_of::<WTA_OPTIONS>() as u32,
            )
            .is_ok();
            // Windows 11 keeps drawing the caption icon after NODRAWICON once
            // the caption color is custom. The small icon is the one in the
            // caption. The big icon stays, so the taskbar keeps it. A frame
            // change makes the nodraw flags take effect.
            clear_caption_icon(hwnd);
            let _ = windows::Win32::UI::WindowsAndMessaging::SetWindowPos(
                hwnd,
                HWND(std::ptr::null_mut()),
                0,
                0,
                0,
                0,
                windows::Win32::UI::WindowsAndMessaging::SWP_NOMOVE
                    | windows::Win32::UI::WindowsAndMessaging::SWP_NOSIZE
                    | windows::Win32::UI::WindowsAndMessaging::SWP_NOZORDER
                    | windows::Win32::UI::WindowsAndMessaging::SWP_NOACTIVATE
                    | windows::Win32::UI::WindowsAndMessaging::SWP_FRAMECHANGED,
            );
            border_ok && caption_ok && text_ok && hidden_ok
        }
    }
    #[cfg(not(windows))]
    {
        let _ = (window, red, green, blue);
        false
    }
}

/// Drop the caption's small icon. The big icon stays, so the taskbar keeps it.
#[cfg(windows)]
fn clear_caption_icon(hwnd: windows::Win32::Foundation::HWND) {
    use windows::Win32::Foundation::{LPARAM, WPARAM};
    use windows::Win32::UI::WindowsAndMessaging::{SendMessageW, ICON_SMALL, WM_SETICON};
    unsafe {
        let _ = SendMessageW(hwnd, WM_SETICON, WPARAM(usize::from(ICON_SMALL)), LPARAM(0));
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn caption_colors_pack_ground_and_a_contrasting_text() {
        assert_eq!(super::colorref(0xFC, 0xF3, 0xE4), 0x00E4_F3FC);
        assert_eq!(super::colorref(0x22, 0x1C, 0x19), 0x0019_1C22);
        assert_eq!(super::caption_text_color(0xFE, 0xFC, 0xF8), 0x001A_1A1A);
        assert_eq!(super::caption_text_color(0x28, 0x1E, 0x17), 0x00E4_F3FC);
    }

    #[test]
    fn absent_window_refuses_caption_color() {
        assert!(!super::apply_caption_colors(0, 0xFE, 0xFC, 0xF8));
        assert!(!super::apply_caption_colors(0, 0x28, 0x1E, 0x17));
    }
}
