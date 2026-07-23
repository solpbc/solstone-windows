// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

use std::env;
use std::fs;
use std::path::Path;
use std::process::Command;

const TOKEN_ENV: &str = "SOLSTONE_LIVE_TOKEN_NEEDLE";
const LOCATOR_ENV: &str = "SOLSTONE_LIVE_LOCATOR_NEEDLE";
const TOKEN_RULE: &str = "must match ^[0-9a-f]{16}$";
const LOCATOR_RULE: &str = "must be non-empty and contain no ASCII whitespace or control bytes";
const REDACTED_NEEDLE: &str = "<redacted-needle>";

fn contains_subslice(haystack: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty()
        && haystack
            .windows(needle.len())
            .any(|window| window == needle)
}

fn read_present_env(name: &'static str, rule: &str) -> Option<Vec<u8>> {
    let value = env::var_os(name)?;
    Some(
        value
            .into_string()
            .unwrap_or_else(|_| panic!("{name} {rule}"))
            .into_bytes(),
    )
}

fn redact_needles(path: &str, needles: &[(&str, Vec<u8>)]) -> String {
    let mut redacted = path.to_owned();
    for (_, needle) in needles {
        let needle = std::str::from_utf8(needle).expect("validated needle must be UTF-8");
        redacted = redacted.replace(needle, REDACTED_NEEDLE);
    }
    redacted
}

#[test]
fn offending_paths_redact_all_active_needles() {
    let needles = vec![("first", b"alpha".to_vec()), ("second", b"beta".to_vec())];

    assert_eq!(
        redact_needles("alpha/alpha.txt", &needles),
        "<redacted-needle>/<redacted-needle>.txt"
    );
    assert_eq!(
        redact_needles("alpha/beta/alpha-beta.txt", &needles),
        "<redacted-needle>/<redacted-needle>/<redacted-needle>-<redacted-needle>.txt"
    );
    assert_eq!(
        redact_needles("fixtures/clean.txt", &needles),
        "fixtures/clean.txt"
    );
}

#[test]
fn out_of_band_needles_are_absent_from_the_tracked_tree() {
    let token = read_present_env(TOKEN_ENV, TOKEN_RULE);
    let locator = read_present_env(LOCATOR_ENV, LOCATOR_RULE);
    if token.is_none() && locator.is_none() {
        eprintln!("skipping tracked-tree needle scan: no needle environment variables are set");
        return;
    }

    let mut needles = Vec::new();
    if let Some(token) = token {
        if token.len() != 16
            || !token
                .iter()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(byte))
        {
            panic!("{TOKEN_ENV} {TOKEN_RULE}");
        }
        needles.push((TOKEN_ENV, token));
    }
    if let Some(locator) = locator {
        if locator.is_empty()
            || locator
                .iter()
                .any(|byte| byte.is_ascii_whitespace() || byte.is_ascii_control())
        {
            panic!("{LOCATOR_ENV} {LOCATOR_RULE}");
        }
        needles.push((LOCATOR_ENV, locator));
    }

    for (name, needle) in &needles {
        let mut control = b"prefix-".to_vec();
        control.extend_from_slice(needle);
        control.extend_from_slice(b"-suffix");
        assert!(
            contains_subslice(&control, needle),
            "{name} negative control failed"
        );
    }

    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask manifest directory must have a workspace parent");
    let output = Command::new("git")
        .args(["ls-files", "-z"])
        .current_dir(workspace_root)
        .env_remove(TOKEN_ENV)
        .env_remove(LOCATOR_ENV)
        .output()
        .expect("git ls-files must run for the tracked-tree needle scan");
    assert!(
        output.status.success(),
        "git ls-files failed for the tracked-tree needle scan"
    );

    let mut matches = vec![Vec::new(); needles.len()];
    for path_bytes in output.stdout.split(|byte| *byte == 0) {
        if path_bytes.is_empty() {
            continue;
        }
        let path = std::str::from_utf8(path_bytes)
            .expect("git ls-files returned a non-UTF-8 repository path");
        let Ok(bytes) = fs::read(workspace_root.join(path)) else {
            continue;
        };
        for (index, (_, needle)) in needles.iter().enumerate() {
            if contains_subslice(&bytes, needle) {
                matches[index].push(path);
            }
        }
    }

    for ((name, _), offending_paths) in needles.iter().zip(matches) {
        let redacted_paths = offending_paths
            .iter()
            .map(|path| redact_needles(path, &needles))
            .collect::<Vec<_>>();
        assert!(
            offending_paths.is_empty(),
            "{name}: {} offending tracked paths:\n{}",
            offending_paths.len(),
            redacted_paths.join("\n")
        );
    }
}
