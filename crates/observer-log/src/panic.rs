// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Panic line formatting and hook installation.

use std::io::Write;
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use crate::record::{capped_len_str, encode, Field, FieldValue, Header};
use crate::writer::RotatingFileWriter;

/// Build the single panic log line.
pub fn panic_line(
    now: SystemTime,
    thread: &str,
    file: Option<&str>,
    line: Option<u32>,
    column: Option<u32>,
    message: &str,
) -> String {
    let fields = [
        Field {
            name: std::borrow::Cow::Borrowed("thread"),
            value: FieldValue::Len(capped_len_str(thread)),
        },
        Field {
            name: std::borrow::Cow::Borrowed("message"),
            value: FieldValue::Len(capped_len_str(message)),
        },
    ];
    let header = Header {
        now,
        level: "PANIC",
        target: "-",
        module: None,
        file,
        line,
        column,
        spans: &[],
    };
    encode(&header, &fields)
}

/// Install a panic hook that writes through the existing rotating writer.
pub fn install_panic_hook(writer: Arc<Mutex<RotatingFileWriter>>) {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let message = panic_message(info);
        let location = info.location();
        let file = location.map(|l| l.file());
        let line = location.map(|l| l.line());
        let column = location.map(|l| l.column());
        let thread = std::thread::current();
        let thread_name = thread.name().unwrap_or("");
        let line_text = panic_line(SystemTime::now(), thread_name, file, line, column, &message);

        let mut guard = writer.lock().unwrap_or_else(|error| error.into_inner());
        let _ = guard.write_all(line_text.as_bytes());
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
        String::new()
    }
}
