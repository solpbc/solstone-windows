// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Panic line formatting and hook installation.

use std::io::Write;
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use crate::time::format_rfc3339_utc;
use crate::writer::RotatingFileWriter;

/// Build the single panic log line.
pub fn panic_line(now: SystemTime, thread: &str, location: Option<&str>, message: &str) -> String {
    format!(
        "{} PANIC thread={} location={} message={}\n",
        format_rfc3339_utc(now),
        thread,
        location.unwrap_or("<unknown>"),
        message
    )
}

/// Install a panic hook that writes through the existing rotating writer.
pub fn install_panic_hook(writer: Arc<Mutex<RotatingFileWriter>>) {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let message = panic_message(info);
        let location = info.location().map(|location| {
            format!(
                "{}:{}:{}",
                location.file(),
                location.line(),
                location.column()
            )
        });
        let thread = std::thread::current();
        let thread_name = thread.name().unwrap_or("<unnamed>");
        let line = panic_line(
            SystemTime::now(),
            thread_name,
            location.as_deref(),
            &message,
        );

        let mut guard = writer.lock().unwrap_or_else(|error| error.into_inner());
        let _ = guard.write_all(line.as_bytes());
        let _ = guard.flush();

        previous(info);
    }));
}

fn panic_message(info: &std::panic::PanicHookInfo<'_>) -> String {
    if let Some(message) = info.payload().downcast_ref::<&str>() {
        (*message).to_string()
    } else if let Some(message) = info.payload().downcast_ref::<String>() {
        message.clone()
    } else {
        "<non-string panic>".to_string()
    }
}
