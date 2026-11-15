// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! User preferences for an unpackaged desktop app; no package identity required.

#[cfg(windows)]
pub fn preferred_languages() -> Vec<String> {
    use windows::System::UserProfile::GlobalizationPreferences;
    if let Ok(languages) = GlobalizationPreferences::Languages() {
        if let Ok(size) = languages.Size() {
            let result: Vec<String> = (0..size)
                .filter_map(|i| languages.GetAt(i).ok().map(|s| s.to_string_lossy()))
                .collect();
            if !result.is_empty() {
                return result;
            }
        }
    }
    use windows::Win32::Globalization::{GetUserPreferredUILanguages, MUI_LANGUAGE_NAME};
    // Settings can change between the size and fill calls. Retry with a newly
    // sized buffer; never treat a partial MULTI_SZ as the user's complete list.
    for _ in 0..3 {
        let mut count = 0;
        let mut length = 0;
        unsafe {
            if GetUserPreferredUILanguages(
                MUI_LANGUAGE_NAME,
                &mut count,
                windows::core::PWSTR::null(),
                &mut length,
            )
            .is_err()
            {
                break;
            }
            let mut buffer = vec![0u16; length as usize];
            if GetUserPreferredUILanguages(
                MUI_LANGUAGE_NAME,
                &mut count,
                windows::core::PWSTR(buffer.as_mut_ptr()),
                &mut length,
            )
            .is_ok()
            {
                return buffer
                    .split(|c| *c == 0)
                    .filter(|s| !s.is_empty())
                    .filter_map(|s| String::from_utf16(s).ok())
                    .collect();
            }
        }
    }
    Vec::new()
}

#[cfg(not(windows))]
pub fn preferred_languages() -> Vec<String> {
    Vec::new()
}

#[cfg(windows)]
pub fn format_locale() -> String {
    use windows::Win32::Globalization::{GetUserDefaultLocaleName, MAX_LOCALE_NAME};
    let mut buffer = [0u16; MAX_LOCALE_NAME as usize];
    let size = unsafe { GetUserDefaultLocaleName(&mut buffer) };
    if size > 1 {
        String::from_utf16_lossy(&buffer[..size as usize - 1])
    } else {
        observer_model::i18n::locale().to_string()
    }
}

#[cfg(not(windows))]
pub fn format_locale() -> String {
    observer_model::i18n::locale().to_string()
}

#[cfg(windows)]
pub fn webview_environment(
    data_directory: &std::path::Path,
    arguments: &str,
) -> windows_core::Result<webview2_com::Microsoft::Web::WebView2::Win32::ICoreWebView2Environment> {
    use webview2_com::Microsoft::Web::WebView2::Win32::{
        CreateCoreWebView2EnvironmentWithOptions, ICoreWebView2EnvironmentOptions,
        COREWEBVIEW2_SCROLLBAR_STYLE_FLUENT_OVERLAY,
    };
    use webview2_com::{
        CoreWebView2EnvironmentOptions, CreateCoreWebView2EnvironmentCompletedHandler,
    };
    use windows_core::{Error, HRESULT, HSTRING, PCWSTR};
    let options = CoreWebView2EnvironmentOptions::default();
    let (sender, receiver) = std::sync::mpsc::channel();
    // These options mirror Wry's factory. The language is the negotiated catalog
    // locale; browser extensions remain disabled and existing flags are retained.
    unsafe {
        options.set_additional_browser_arguments(arguments.to_string());
        options.set_are_browser_extensions_enabled(false);
        options.set_language(observer_model::i18n::locale().to_string());
        options.set_scroll_bar_style(COREWEBVIEW2_SCROLLBAR_STYLE_FLUENT_OVERLAY);
        CreateCoreWebView2EnvironmentWithOptions(
            PCWSTR::null(),
            &HSTRING::from(data_directory.as_os_str()),
            &ICoreWebView2EnvironmentOptions::from(options),
            &CreateCoreWebView2EnvironmentCompletedHandler::create(Box::new(
                move |result, environment| {
                    let result = result.and_then(|_| {
                        environment
                            .ok_or_else(|| Error::from_hresult(HRESULT(0x80004003_u32 as i32)))
                    });
                    sender
                        .send(result)
                        .map_err(|_| Error::from_hresult(HRESULT(0x8000FFFF_u32 as i32)))
                },
            )),
        )?;
    }
    // Must run on Tauri's UI STA: pump COM callbacks while creation completes.
    webview2_com::wait_with_pump(receiver)
        .map_err(|_| Error::from_hresult(HRESULT(0x80004005_u32 as i32)))?
}
