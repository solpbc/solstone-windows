// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Writes executable test fixtures without opening them for writing in the
//! test process.
//!
//! Under parallel test threads on Unix, a child spawned on another thread
//! inherits every open file descriptor until it execs. If the test process
//! opens an executable script for writing, another thread's in-flight child
//! can hold that write descriptor past `close()` and cause `execve()` to fail
//! with `ETXTBSY` ("Text file busy"). Delegating the write to a short-lived
//! `/bin/sh` child and renaming after it exits guarantees the test process
//! never holds a write descriptor to the executable inode.

#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;

pub fn write_executable_script(path: &Path, body: &str, mode: u32) {
    let mut staging = path.as_os_str().to_owned();
    staging.push(".staging");
    let status = Command::new("/bin/sh")
        .args(["-c", "printf -- '%s' \"$2\" > \"$1\"", "sh"])
        .arg(&staging)
        .arg(body)
        .status()
        .expect("start executable fixture writer");
    assert!(
        status.success(),
        "executable fixture writer exited with {status}"
    );
    fs::set_permissions(&staging, fs::Permissions::from_mode(mode))
        .expect("set executable fixture permissions");
    fs::rename(&staging, path).expect("place executable fixture");
}
