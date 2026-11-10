// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Caption colors for the settings window.
//!
//! Windows 11 accepts a caption, border, and caption-text color. Windows 10
//! refuses those attributes; the caller ignores the failure and the system
//! caption stays. The buttons stay the ones Windows draws.

/// Pack a color as COLORREF, `0x00BBGGRR`.
pub fn colorref(red: u8, green: u8, blue: u8) -> u32 {
    u32::from(blue) << 16 | u32::from(green) << 8 | u32::from(red)
}

/// Caption, text, and border for the light or dark ground.
pub fn caption_colors(dark: bool) -> (u32, u32, u32) {
    if dark {
        let ground = colorref(0x22, 0x1C, 0x19);
        let text = colorref(0xFC, 0xF3, 0xE4);
        (ground, text, ground)
    } else {
        let ground = colorref(0xFC, 0xF3, 0xE4);
        let text = colorref(0x1A, 0x1A, 0x1A);
        (ground, text, ground)
    }
}

/// Paint the native caption. Returns false when the handle is absent or the
/// OS refuses the attributes.
#[allow(unsafe_code)]
pub fn apply_caption_colors(window: isize, dark: bool) -> bool {
    #[cfg(windows)]
    {
        use windows::Win32::Foundation::HWND;
        use windows::Win32::Graphics::Dwm::{
            DwmSetWindowAttribute, DWMWA_BORDER_COLOR, DWMWA_CAPTION_COLOR, DWMWA_TEXT_COLOR,
        };

        if window == 0 {
            return false;
        }
        let (caption, text, border) = caption_colors(dark);
        let hwnd = HWND(window as *mut std::ffi::c_void);
        // The attribute call writes three local colors through an opaque handle.
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
            border_ok && caption_ok && text_ok
        }
    }
    #[cfg(not(windows))]
    {
        let _ = (window, dark);
        false
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn caption_colors_pack_ground_and_text() {
        let (caption, text, border) = super::caption_colors(false);
        assert_eq!(caption, 0x00E4_F3FC);
        assert_eq!(text, 0x001A_1A1A);
        assert_eq!(border, caption);

        let (caption, text, border) = super::caption_colors(true);
        assert_eq!(caption, 0x0019_1C22);
        assert_eq!(text, 0x00E4_F3FC);
        assert_eq!(border, caption);
    }

    #[test]
    fn absent_window_refuses_caption_color() {
        assert!(!super::apply_caption_colors(0, false));
        assert!(!super::apply_caption_colors(0, true));
    }
}
