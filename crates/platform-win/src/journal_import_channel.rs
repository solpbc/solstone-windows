// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! The journal window's one channel into the app: import handoff.
//!
//! The page asks with a request to one reserved address on its own origin. The
//! app answers that request inside the window, before it reaches the network,
//! so it is never a Tauri page call and never reaches the journal. A WebView2
//! message would share its event with wry's handler, which fails on any message
//! that isn't a string and so stops the handlers after it.
//!
//! Every call here runs on the window's UI thread: the event handler, and the
//! answer, which the caller posts back to that thread.

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};

use journal_window::import_host::{endpoint, HOST_HEADER, REPLY_HEADER};
use journal_window::JournalOrigin;
use webview2_com::Microsoft::Web::WebView2::Win32::{
    ICoreWebView2, ICoreWebView2Controller, ICoreWebView2Deferral, ICoreWebView2Environment,
    ICoreWebView2WebResourceRequestedEventArgs, ICoreWebView2_22,
    COREWEBVIEW2_WEB_RESOURCE_CONTEXT_ALL, COREWEBVIEW2_WEB_RESOURCE_REQUEST_SOURCE_KINDS_DOCUMENT,
};
use webview2_com::{CoTaskMemPWSTR, WebResourceRequestedEventHandler};
use windows_core::{Interface, HSTRING, PWSTR};

type Pending = (
    ICoreWebView2WebResourceRequestedEventArgs,
    ICoreWebView2Deferral,
);

thread_local! {
    static PENDING: RefCell<HashMap<u64, Pending>> = RefCell::new(HashMap::new());
}
static NEXT: AtomicU64 = AtomicU64::new(1);

fn string(read: impl FnOnce(&mut PWSTR) -> windows_core::Result<()>) -> Option<String> {
    let mut raw = PWSTR::null();
    let result = read(&mut raw);
    let value = CoTaskMemPWSTR::from(raw);
    result.ok().map(|()| value.to_string())
}

/// Register the reserved address. `receive` gets each request's id and its
/// message header; the request waits until [`respond`] answers that id.
pub fn install(
    controller: &ICoreWebView2Controller,
    environment: ICoreWebView2Environment,
    origin: &JournalOrigin,
    receive: impl Fn(u64, String) + 'static,
) -> bool {
    let view = match unsafe { controller.CoreWebView2() } {
        Ok(view) if !view.as_raw().is_null() => view,
        _ => return false,
    };
    let Ok(extended) = view.cast::<ICoreWebView2_22>() else {
        return false;
    };
    let address = endpoint(origin);
    if unsafe {
        extended.AddWebResourceRequestedFilterWithRequestSourceKinds(
            &HSTRING::from(address.as_str()),
            COREWEBVIEW2_WEB_RESOURCE_CONTEXT_ALL,
            COREWEBVIEW2_WEB_RESOURCE_REQUEST_SOURCE_KINDS_DOCUMENT,
        )
    }
    .is_err()
    {
        return false;
    }
    let handler = WebResourceRequestedEventHandler::create(Box::new(
        move |_sender: Option<ICoreWebView2>,
              args: Option<ICoreWebView2WebResourceRequestedEventArgs>| {
            let Some(args) = args else {
                return Ok(());
            };
            let Ok(request) = (unsafe { args.Request() }) else {
                return Ok(());
            };
            // Every handler sees every filtered request; answer only this one.
            if string(|raw| unsafe { request.Uri(raw) }).as_deref() != Some(address.as_str()) {
                return Ok(());
            }
            let Ok(deferral) = (unsafe { args.GetDeferral() }) else {
                return Ok(());
            };
            let header = unsafe { request.Headers() }.ok().and_then(|headers| {
                string(|raw| unsafe { headers.GetHeader(&HSTRING::from(HOST_HEADER), raw) })
            });
            let method = string(|raw| unsafe { request.Method(raw) });
            let id = NEXT.fetch_add(1, Ordering::Relaxed);
            PENDING.with(|pending| pending.borrow_mut().insert(id, (args.clone(), deferral)));
            match (method.as_deref(), header) {
                (Some("POST"), Some(header)) => receive(id, header),
                _ => {
                    // Refused here, on this thread, without the app.
                    PENDING.with(|pending| {
                        if let Some((args, deferral)) = pending.borrow_mut().remove(&id) {
                            finish(&environment, &args, &deferral, None);
                        }
                    });
                }
            }
            Ok(())
        },
    ));
    let mut token = 0_i64;
    unsafe { view.add_WebResourceRequested(&handler, &mut token) }.is_ok()
}

fn finish(
    environment: &ICoreWebView2Environment,
    args: &ICoreWebView2WebResourceRequestedEventArgs,
    deferral: &ICoreWebView2Deferral,
    reply: Option<&str>,
) -> bool {
    let (status, reason, headers) = match reply {
        Some(reply) => (
            200,
            "OK",
            format!(
                "Content-Type: text/plain\r\nCache-Control: no-store\r\n{REPLY_HEADER}: {reply}"
            ),
        ),
        None => (403, "Forbidden", "Cache-Control: no-store".to_owned()),
    };
    let response = unsafe {
        environment.CreateWebResourceResponse(
            None,
            status,
            &HSTRING::from(reason),
            &HSTRING::from(headers.as_str()),
        )
    }
    .ok();
    let set = response.is_some_and(|response| unsafe { args.SetResponse(&response) }.is_ok());
    let _ = unsafe { deferral.Complete() };
    set
}

/// Answer one waiting request: `Some` carries the reply header's value, `None`
/// refuses it. Call on the window's UI thread.
pub fn respond(environment: &ICoreWebView2Environment, id: u64, reply: Option<&str>) -> bool {
    let Some((args, deferral)) = PENDING.with(|pending| pending.borrow_mut().remove(&id)) else {
        return false;
    };
    finish(environment, &args, &deferral, reply)
}
