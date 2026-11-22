// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

use journal_window::{
    dispatch_document, offline_redirect_url, DocumentDispatch, DocumentFault, FilterInstall,
    JournalOrigin,
};
use webview2_com::Microsoft::Web::WebView2::Win32::{
    ICoreWebView2, ICoreWebView2Controller, ICoreWebView2Environment,
    ICoreWebView2NavigationCompletedEventArgs, ICoreWebView2NavigationCompletedEventArgs2,
    ICoreWebView2WebResourceRequestedEventArgs, ICoreWebView2_22,
    COREWEBVIEW2_WEB_RESOURCE_CONTEXT_DOCUMENT, COREWEBVIEW2_WEB_RESOURCE_REQUEST_SOURCE_KINDS_ALL,
};
use webview2_com::{
    CoTaskMemPWSTR, NavigationCompletedEventHandler, WebResourceRequestedEventHandler,
};
use windows_core::{Error, Interface, HRESULT, HSTRING, PWSTR};

pub fn install_document_filter(
    controller: ICoreWebView2Controller,
    environment: ICoreWebView2Environment,
    origin: JournalOrigin,
    terminate: impl Fn(DocumentFault) + Send + 'static,
) -> FilterInstall {
    let view = match unsafe { controller.CoreWebView2() } {
        Ok(view) if !view.as_raw().is_null() => view,
        _ => return FilterInstall::RegistrationFailed,
    };
    let extended = match view.cast::<ICoreWebView2_22>() {
        Ok(extended) => extended,
        Err(_) => return FilterInstall::MissingInterface,
    };

    let filter = HSTRING::from("*");
    if unsafe {
        extended.AddWebResourceRequestedFilterWithRequestSourceKinds(
            &filter,
            COREWEBVIEW2_WEB_RESOURCE_CONTEXT_DOCUMENT,
            COREWEBVIEW2_WEB_RESOURCE_REQUEST_SOURCE_KINDS_ALL,
        )
    }
    .is_err()
    {
        return FilterInstall::RegistrationFailed;
    }

    let reason = HSTRING::from("Forbidden");
    let headers = HSTRING::new();
    let handler = WebResourceRequestedEventHandler::create(Box::new(
        move |sender: Option<ICoreWebView2>,
              args: Option<ICoreWebView2WebResourceRequestedEventArgs>| {
            let request_uri = match args
                .as_ref()
                .and_then(|args| unsafe { args.Request().ok() })
            {
                None => None,
                Some(request) => {
                    let mut raw = PWSTR::null();
                    let result = unsafe { request.Uri(&mut raw) };
                    let uri = CoTaskMemPWSTR::from(raw);
                    match result {
                        Ok(()) => Some(Ok(uri.to_string())),
                        Err(_) => Some(Err(())),
                    }
                }
            };
            let request_uri = request_uri.as_ref().map(|uri| match uri {
                Ok(uri) => Ok(uri.as_str()),
                Err(()) => Err(()),
            });

            let dispatch = dispatch_document(
                &origin,
                request_uri,
                || unsafe {
                    environment
                        .CreateWebResourceResponse(None, 403, &reason, &headers)
                        .map_err(|_| ())
                },
                |response| {
                    let Some(args) = args.as_ref() else {
                        return Err(());
                    };
                    unsafe { args.SetResponse(&response).map_err(|_| ()) }
                },
                |fault| {
                    if let Some(sender) = sender.as_ref() {
                        let _ = unsafe { sender.Stop() };
                    }
                    let view_closed = unsafe { controller.Close() }.is_ok();
                    tracing::warn!(
                        target: "window",
                        label = "journal",
                        fault = fault.token(),
                        view_closed,
                        "journal document request fault"
                    );
                    terminate(fault);
                },
            );

            match dispatch {
                DocumentDispatch::Allow | DocumentDispatch::Denied => Ok(()),
                DocumentDispatch::Terminate(_) => {
                    Err(Error::new(HRESULT(0x8000_4005u32 as i32), ""))
                }
            }
        },
    ));

    let mut token = 0_i64;
    if unsafe { view.add_WebResourceRequested(&handler, &mut token) }.is_err() {
        return FilterInstall::RegistrationFailed;
    }

    FilterInstall::Comprehensive
}

/// Install a main-frame `NavigationCompleted` listener that replaces raw
/// `502 journal unreachable` answers on `origin` with the same-origin
/// `/_solstone/offline?next=...` reconnect page.
pub fn install_offline_handler(
    controller: &ICoreWebView2Controller,
    origin: JournalOrigin,
) -> bool {
    let view = match unsafe { controller.CoreWebView2() } {
        Ok(view) if !view.as_raw().is_null() => view,
        _ => return false,
    };
    let handler = NavigationCompletedEventHandler::create(Box::new(
        move |sender: Option<ICoreWebView2>,
              args: Option<ICoreWebView2NavigationCompletedEventArgs>| {
            let (Some(sender), Some(args)) = (sender, args) else {
                return Ok(());
            };
            let Ok(args2) = args.cast::<ICoreWebView2NavigationCompletedEventArgs2>() else {
                return Ok(());
            };
            let mut status: i32 = 0;
            if unsafe { args2.HttpStatusCode(&mut status) }.is_err() || status != 502 {
                return Ok(());
            }
            let mut raw = PWSTR::null();
            let result = unsafe { sender.Source(&mut raw) };
            let source = CoTaskMemPWSTR::from(raw);
            if result.is_err() {
                return Ok(());
            }
            if let Some(target) = offline_redirect_url(&origin, &source.to_string()) {
                let _ = unsafe { sender.Navigate(&HSTRING::from(target.as_str())) };
            }
            Ok(())
        },
    ));
    let mut token = 0_i64;
    unsafe { view.add_NavigationCompleted(&handler, &mut token) }.is_ok()
}
