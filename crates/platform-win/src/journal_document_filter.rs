// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

use journal_window::{
    dispatch_document, DocumentDispatch, DocumentFault, FilterInstall, JournalOrigin,
};
use webview2_com::Microsoft::Web::WebView2::Win32::{
    ICoreWebView2, ICoreWebView2Controller, ICoreWebView2Environment,
    ICoreWebView2WebResourceRequestedEventArgs, ICoreWebView2_22,
    COREWEBVIEW2_WEB_RESOURCE_CONTEXT_DOCUMENT,
    COREWEBVIEW2_WEB_RESOURCE_REQUEST_SOURCE_KINDS_DOCUMENT,
};
use webview2_com::{CoTaskMemPWSTR, WebResourceRequestedEventHandler};
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
            COREWEBVIEW2_WEB_RESOURCE_REQUEST_SOURCE_KINDS_DOCUMENT,
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
                    tracing::warn!(
                        target: "window",
                        label = "journal",
                        fault = fault.token(),
                        "journal document request terminated"
                    );
                    if let Some(sender) = sender.as_ref() {
                        let _ = unsafe { sender.Stop() };
                    }
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
