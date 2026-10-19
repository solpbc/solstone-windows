// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Every app window shares one WebView2 user data folder. WebView2 refuses to start
//! a webview whose environment options differ from a webview already running in
//! that folder, so a window built with its own options cannot open while another
//! window is open. Each window must take its options from the one shared helper.

use std::fs;
use std::path::{Path, PathBuf};

const HELPER: &str = "shared_webview_environment(";
const BUILDER: &str = "WebviewWindowBuilder::new(";
const ENVIRONMENT_OPTIONS: [&str; 5] = [
    ".additional_browser_args(",
    ".scroll_bar_style(",
    ".data_directory(",
    ".browser_extensions_enabled(",
    ".proxy_url(",
];

fn app_sources() -> Vec<(PathBuf, String)> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("src-tauri")
        .join("src");
    let mut pending = vec![root];
    let mut files = Vec::new();
    while let Some(dir) = pending.pop() {
        for entry in fs::read_dir(&dir).expect("read app source dir") {
            let path = entry.expect("app source entry").path();
            if path.is_dir() {
                pending.push(path);
            } else if path.extension().is_some_and(|ext| ext == "rs") {
                let text = fs::read_to_string(&path).expect("read app source");
                files.push((path, text));
            }
        }
    }
    assert!(!files.is_empty(), "no app sources found");
    files
}

fn without_whitespace(text: &str) -> String {
    text.chars().filter(|c| !c.is_whitespace()).collect()
}

#[test]
fn every_window_takes_the_shared_webview_environment() {
    let mut builders = 0;
    for (path, text) in app_sources() {
        let compact = without_whitespace(&text);
        let wrapped = compact.matches(&format!("{HELPER}{BUILDER}")).count();
        let all = compact.matches(BUILDER).count();
        builders += all;
        assert_eq!(
            wrapped,
            all,
            "{}: a WebviewWindowBuilder is not wrapped in {HELPER}..)",
            path.display()
        );
    }
    assert!(
        builders >= 3,
        "expected the settings, about and journal windows"
    );
}

#[test]
fn environment_options_are_set_only_in_the_shared_helper() {
    let mut helper_files = 0;
    for (path, text) in app_sources() {
        let helper_start = text.find("fn shared_webview_environment(");
        if helper_start.is_some() {
            helper_files += 1;
        }
        let helper_end = helper_start.map(|start| {
            start
                + text[start..]
                    .find("\n}\n")
                    .expect("shared helper has an end")
        });
        for option in ENVIRONMENT_OPTIONS {
            for (at, _) in text.match_indices(option) {
                let inside = matches!(
                    (helper_start, helper_end),
                    (Some(start), Some(end)) if at > start && at < end
                );
                assert!(
                    inside,
                    "{}: {option} outside the shared webview environment helper",
                    path.display()
                );
            }
        }
    }
    assert_eq!(
        helper_files, 1,
        "exactly one shared webview environment helper"
    );
}
