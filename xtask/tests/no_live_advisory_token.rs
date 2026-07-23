// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

use std::env;
use std::fs;
use std::path::Path;
use std::process::Command;

const SCAN_MODE_ENV: &str = "SOLSTONE_ADVISORY_NEEDLE_SCAN";
const HEX16_RULE: &str = "must match ^[0-9a-f]{16}$";
const NONEMPTY_NO_CONTROL_RULE: &str =
    "must be non-empty and contain no ASCII whitespace or control bytes";
const REDACTED_NEEDLE: &str = "<redacted-needle>";

struct NeedleSpec {
    label: &'static str,
    env: &'static str,
    rule: &'static str,
    is_valid: fn(&[u8]) -> bool,
}

const NEEDLE_SPECS: [NeedleSpec; 4] = [
    NeedleSpec {
        label: "token",
        env: "SOLSTONE_ADVISORY_NEEDLE_TOKEN",
        rule: HEX16_RULE,
        is_valid: is_hex16,
    },
    NeedleSpec {
        label: "host",
        env: "SOLSTONE_ADVISORY_NEEDLE_HOST",
        rule: NONEMPTY_NO_CONTROL_RULE,
        is_valid: is_nonempty_no_control,
    },
    NeedleSpec {
        label: "path",
        env: "SOLSTONE_ADVISORY_NEEDLE_PATH",
        rule: NONEMPTY_NO_CONTROL_RULE,
        is_valid: is_nonempty_no_control,
    },
    NeedleSpec {
        label: "locator",
        env: "SOLSTONE_ADVISORY_NEEDLE_LOCATOR",
        rule: NONEMPTY_NO_CONTROL_RULE,
        is_valid: is_nonempty_no_control,
    },
];

fn is_hex16(value: &[u8]) -> bool {
    value.len() == 16
        && value
            .iter()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(byte))
}

fn is_nonempty_no_control(value: &[u8]) -> bool {
    !value.is_empty()
        && !value
            .iter()
            .any(|byte| byte.is_ascii_whitespace() || byte.is_ascii_control())
}

fn contains_subslice(haystack: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty()
        && haystack
            .windows(needle.len())
            .any(|window| window == needle)
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
    let needles = vec![
        ("token", b"synthetic-token".to_vec()),
        ("host", b"synthetic-host".to_vec()),
        ("path", b"synthetic-path".to_vec()),
        ("locator", b"synthetic-locator".to_vec()),
    ];

    assert_eq!(
        redact_needles("synthetic-token/synthetic-token.txt", &needles),
        "<redacted-needle>/<redacted-needle>.txt"
    );
    assert_eq!(
        redact_needles(
            "synthetic-host/synthetic-path/synthetic-locator-synthetic-token.txt",
            &needles
        ),
        "<redacted-needle>/<redacted-needle>/<redacted-needle>-<redacted-needle>.txt"
    );
    assert_eq!(
        redact_needles("fixtures/clean.txt", &needles),
        "fixtures/clean.txt"
    );
}

#[test]
fn is_hex16_accepts_only_sixteen_lowercase_hex_bytes() {
    assert!(is_hex16(b"0123456789abcdef"));

    for rejected in [
        b"0123456789abcde".as_slice(),
        b"0123456789abcdef0".as_slice(),
        b"0123456789abcdeF".as_slice(),
        b"0123456789abcdeg".as_slice(),
        b"".as_slice(),
    ] {
        assert!(!is_hex16(rejected));
    }
}

#[test]
fn is_nonempty_no_control_rejects_ascii_whitespace_and_controls() {
    for accepted in [
        b"mirror.example.invalid".as_slice(),
        b"/rustsec/advisory-db".as_slice(),
        b"ssh://mirror.example.invalid/advisory-db.git".as_slice(),
    ] {
        assert!(is_nonempty_no_control(accepted));
    }

    for rejected in [
        b"".as_slice(),
        b"mirror example.invalid".as_slice(),
        b"mirror\t.example.invalid".as_slice(),
        b"/rustsec/\nadvisory-db".as_slice(),
        b"mirror\0.example.invalid".as_slice(),
        b"mirror\x1f.example.invalid".as_slice(),
    ] {
        assert!(!is_nonempty_no_control(rejected));
    }
}

#[test]
fn out_of_band_needles_are_absent_from_the_tracked_tree() {
    if env::var_os(SCAN_MODE_ENV).is_none() {
        eprintln!("skipping tracked-tree needle scan: {SCAN_MODE_ENV} is not set");
        return;
    }

    let mut needles = Vec::with_capacity(NEEDLE_SPECS.len());
    for spec in &NEEDLE_SPECS {
        let value = env::var_os(spec.env).unwrap_or_else(|| {
            panic!(
                "{} needle: {} is required when {} is set; value {}",
                spec.label, spec.env, SCAN_MODE_ENV, spec.rule
            )
        });
        let value = value.into_string().unwrap_or_else(|_| {
            panic!(
                "{} needle: {} is malformed; value {}",
                spec.label, spec.env, spec.rule
            )
        });
        let value = value.into_bytes();
        if !(spec.is_valid)(&value) {
            panic!(
                "{} needle: {} is malformed; value {}",
                spec.label, spec.env, spec.rule
            );
        }
        needles.push((spec.label, value));
    }

    for (label, needle) in &needles {
        let mut control = b"prefix-".to_vec();
        control.extend_from_slice(needle);
        control.extend_from_slice(b"-suffix");
        assert!(
            contains_subslice(&control, needle),
            "{label} negative control failed"
        );
    }

    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask manifest directory must have a workspace parent");
    let mut git = Command::new("git");
    git.args(["ls-files", "-z"])
        .current_dir(workspace_root)
        .env_remove(SCAN_MODE_ENV);
    for spec in &NEEDLE_SPECS {
        git.env_remove(spec.env);
    }
    let output = git
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

    for ((label, _), offending_paths) in needles.iter().zip(matches) {
        let redacted_paths = offending_paths
            .iter()
            .map(|path| redact_needles(path, &needles))
            .collect::<Vec<_>>();
        assert!(
            offending_paths.is_empty(),
            "{label}: {} offending tracked paths:\n{}",
            offending_paths.len(),
            redacted_paths.join("\n")
        );
    }
}
