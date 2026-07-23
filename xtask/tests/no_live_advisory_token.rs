// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

use std::env;
use std::fmt;
use std::fs;
use std::io::{ErrorKind, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

const SCAN_MODE_ENV: &str = "SOLSTONE_ADVISORY_NEEDLE_SCAN";
const HEX16_RULE: &str = "must match ^[0-9a-f]{16}$";
const NONEMPTY_NO_CONTROL_RULE: &str =
    "must be non-empty and contain no ASCII whitespace or control bytes";
const REDACTED_NEEDLE: &str = "<redacted-needle>";
const WITHHELD_PATH: &str = "<withheld-path>";

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

fn redact_needles(path: &[u8], needles: &[(&str, Vec<u8>)]) -> String {
    let mut redacted = path.to_vec();
    for (_, needle) in needles {
        let mut replaced = Vec::with_capacity(redacted.len());
        let mut cursor = 0;
        while cursor < redacted.len() {
            if redacted[cursor..].starts_with(needle) {
                replaced.extend_from_slice(REDACTED_NEEDLE.as_bytes());
                cursor += needle.len();
            } else {
                replaced.push(redacted[cursor]);
                cursor += 1;
            }
        }
        redacted = replaced;
    }
    let rendered = String::from_utf8_lossy(&redacted).into_owned();
    for (_, needle) in needles {
        if contains_subslice(rendered.as_bytes(), needle) {
            return WITHHELD_PATH.to_owned();
        }
    }
    rendered
}

/// Collects one labeled hit per scanned item containing a needle. Index i of
/// the result corresponds to needles[i].
fn collect_needle_matches(
    scanned: &[(&'static str, &[u8], &[u8])],
    needles: &[(&str, Vec<u8>)],
) -> Vec<Vec<(&'static str, Vec<u8>)>> {
    let mut matches = vec![Vec::new(); needles.len()];
    for (location, path, haystack) in scanned {
        for (index, (_, needle)) in needles.iter().enumerate() {
            if contains_subslice(haystack, needle) {
                matches[index].push((*location, path.to_vec()));
            }
        }
    }
    matches
}

fn render_matches(matches: &[(&'static str, Vec<u8>)], needles: &[(&str, Vec<u8>)]) -> Vec<String> {
    matches
        .iter()
        .map(|(location, path)| format!("{location}: {}", redact_needles(path, needles)))
        .collect()
}

#[test]
fn redact_needles_redacts_all_active_needles_before_lossy_display() {
    let needles = vec![
        ("token", b"synthetic-token".to_vec()),
        ("host", b"synthetic-host".to_vec()),
        ("path", b"synthetic-path".to_vec()),
        ("locator", b"synthetic-locator".to_vec()),
    ];

    assert_eq!(
        redact_needles(b"synthetic-token/synthetic-token.txt", &needles),
        "<redacted-needle>/<redacted-needle>.txt"
    );
    assert_eq!(
        redact_needles(
            b"synthetic-host/synthetic-path/synthetic-locator-synthetic-token.txt",
            &needles
        ),
        "<redacted-needle>/<redacted-needle>/<redacted-needle>-<redacted-needle>.txt"
    );
    assert_eq!(
        redact_needles(b"fixtures/clean.txt", &needles),
        "fixtures/clean.txt"
    );
    assert_eq!(
        redact_needles(b"prefix-\xff-synthetic-token", &needles),
        "prefix-\u{fffd}-<redacted-needle>"
    );

    let replacement_collision = vec![("host", b"redacted-needle".to_vec())];
    assert_eq!(
        redact_needles(b"prefix-redacted-needle-suffix", &replacement_collision),
        "<withheld-path>"
    );

    let lossy_collision = vec![("host", b"\xef\xbf\xbd".to_vec())];
    assert_eq!(
        redact_needles(b"prefix-\xff-suffix", &lossy_collision),
        "<withheld-path>"
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
fn out_of_band_needles_are_absent_from_the_head_tree() {
    if env::var_os(SCAN_MODE_ENV).is_none() {
        eprintln!("skipping HEAD-tree needle scan: {SCAN_MODE_ENV} is not set");
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
    let records = match head_tree_records(workspace_root) {
        Ok(records) => records,
        Err(error) => panic!("HEAD-tree needle scan error: {error}"),
    };

    let scanned_paths = records
        .iter()
        .map(|record| ("path", record.path.as_slice(), record.path.as_slice()))
        .collect::<Vec<_>>();
    let mut matches = collect_needle_matches(&scanned_paths, &needles);

    let blobs = match read_tree_blobs(workspace_root, &records) {
        Ok(blobs) => blobs,
        Err(error) => panic!("HEAD-tree needle scan error: {error}"),
    };

    let scanned_blobs = blobs
        .iter()
        .map(|blob| ("blob", blob.path.as_slice(), blob.bytes.as_slice()))
        .collect::<Vec<_>>();
    let blob_matches = collect_needle_matches(&scanned_blobs, &needles);
    for (matches, blob_matches) in matches.iter_mut().zip(blob_matches) {
        matches.extend(blob_matches);
    }

    for ((label, _), offending_matches) in needles.iter().zip(matches) {
        let redacted_matches = render_matches(&offending_matches, &needles);
        assert!(
            offending_matches.is_empty(),
            "HEAD-tree needle match found ({label}): {} offending matches:\n{}",
            offending_matches.len(),
            redacted_matches.join("\n")
        );
    }
}

struct TrackedBlob {
    path: Vec<u8>,
    bytes: Vec<u8>,
}

struct TreeRecord {
    mode: String,
    oid: String,
    path: Vec<u8>,
}

#[derive(Debug)]
enum ScanError {
    GitSpawn {
        subcommand: &'static str,
        kind: ErrorKind,
    },
    GitExit {
        subcommand: &'static str,
        status: ExitStatus,
    },
    TempFile {
        operation: &'static str,
        kind: ErrorKind,
    },
    MalformedHeadCommit {
        reason: &'static str,
    },
    UnbornHead,
    EmptyHeadTree,
    MalformedTreeRecord {
        ordinal: usize,
        reason: &'static str,
    },
    UnsupportedTreeMode {
        ordinal: usize,
        mode: String,
    },
    MalformedBatchResponse {
        ordinal: usize,
        reason: &'static str,
    },
    BatchOidMismatch {
        ordinal: usize,
        expected_oid: String,
        actual_oid: String,
    },
    BatchTypeMismatch {
        ordinal: usize,
        oid: String,
    },
    BatchResponseCount {
        expected: usize,
        actual: usize,
    },
    TrailingBatchBytes {
        byte_count: usize,
    },
}

impl fmt::Display for ScanError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::GitSpawn { subcommand, kind } => {
                write!(formatter, "git {subcommand} could not start: {kind}")
            }
            Self::GitExit { subcommand, status } => {
                write!(formatter, "git {subcommand} exited with {status}")
            }
            Self::TempFile { operation, kind } => {
                write!(formatter, "temporary request file {operation} failed: {kind}")
            }
            Self::MalformedHeadCommit { reason } => {
                write!(formatter, "git rev-parse returned a malformed HEAD commit ({reason})")
            }
            Self::UnbornHead => write!(formatter, "repository HEAD is unborn"),
            Self::EmptyHeadTree => write!(formatter, "resolved HEAD commit has an empty tree"),
            Self::MalformedTreeRecord { ordinal, reason } => {
                write!(formatter, "tree record {ordinal} is malformed ({reason})")
            }
            Self::UnsupportedTreeMode { ordinal, mode } => {
                write!(formatter, "tree record {ordinal} has unsupported mode {mode}")
            }
            Self::MalformedBatchResponse { ordinal, reason } => write!(
                formatter,
                "batch response {ordinal} is malformed ({reason})"
            ),
            Self::BatchOidMismatch {
                ordinal,
                expected_oid,
                actual_oid,
            } => write!(
                formatter,
                "batch response {ordinal} returned oid {actual_oid} for requested oid {expected_oid}"
            ),
            Self::BatchTypeMismatch { ordinal, oid } => write!(
                formatter,
                "batch response {ordinal} for oid {oid} is not a blob"
            ),
            Self::BatchResponseCount { expected, actual } => write!(
                formatter,
                "batch response count mismatch: expected {expected}, received {actual}"
            ),
            Self::TrailingBatchBytes { byte_count } => {
                write!(formatter, "batch output has {byte_count} trailing bytes")
            }
        }
    }
}

const GIT_SELECTION_ENV_VARS: [&str; 8] = [
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_INDEX_FILE",
    "GIT_COMMON_DIR",
    "GIT_OBJECT_DIRECTORY",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_NAMESPACE",
    "GIT_REPLACE_REF_BASE",
];

// Let current_dir alone select the repository so ambient Git state cannot
// redirect the scan or let a fixture touch a caller-owned repository.
fn scrub_git_environment(command: &mut Command) {
    command.env_remove(SCAN_MODE_ENV);
    for spec in &NEEDLE_SPECS {
        command.env_remove(spec.env);
    }
    for variable in GIT_SELECTION_ENV_VARS {
        command.env_remove(variable);
    }
}

fn is_valid_oid(value: &[u8]) -> bool {
    matches!(value.len(), 40 | 64)
        && value
            .iter()
            .all(|byte| matches!(*byte, b'0'..=b'9' | b'a'..=b'f'))
}

/// Resolves HEAD once and returns strict raw records from that exact committed
/// tree. It never reads the mutable index or the worktree.
fn head_tree_records(repository_root: &Path) -> Result<Vec<TreeRecord>, ScanError> {
    let mut rev_parse = Command::new("git");
    rev_parse
        .args([
            "--no-replace-objects",
            "rev-parse",
            "--verify",
            "--quiet",
            "HEAD^{commit}",
        ])
        .current_dir(repository_root);
    scrub_git_environment(&mut rev_parse);
    let commit_output = match rev_parse.output() {
        Ok(output) => output,
        Err(error) => {
            return Err(ScanError::GitSpawn {
                subcommand: "rev-parse",
                kind: error.kind(),
            });
        }
    };
    if !commit_output.status.success() {
        if commit_output.status.code() == Some(1) && commit_output.stdout.is_empty() {
            return Err(ScanError::UnbornHead);
        }
        return Err(ScanError::GitExit {
            subcommand: "rev-parse",
            status: commit_output.status,
        });
    }

    let commit_oid_bytes = match commit_output.stdout.strip_suffix(b"\n") {
        Some(value) => value,
        None => {
            return Err(ScanError::MalformedHeadCommit {
                reason: "missing line terminator",
            });
        }
    };
    if commit_oid_bytes.is_empty() {
        return Err(ScanError::MalformedHeadCommit {
            reason: "missing object id",
        });
    }
    if !is_valid_oid(commit_oid_bytes) {
        return Err(ScanError::MalformedHeadCommit {
            reason: "invalid object id",
        });
    }
    let commit_oid: String = commit_oid_bytes
        .iter()
        .map(|byte| char::from(*byte))
        .collect();

    let mut ls_tree = Command::new("git");
    ls_tree
        .args([
            "--no-replace-objects",
            "ls-tree",
            "-r",
            "-z",
            "--full-tree",
            commit_oid.as_str(),
        ])
        .current_dir(repository_root);
    scrub_git_environment(&mut ls_tree);
    let tree_output = match ls_tree.output() {
        Ok(output) => output,
        Err(error) => {
            return Err(ScanError::GitSpawn {
                subcommand: "ls-tree",
                kind: error.kind(),
            });
        }
    };
    if !tree_output.status.success() {
        return Err(ScanError::GitExit {
            subcommand: "ls-tree",
            status: tree_output.status,
        });
    }

    let mut records = Vec::new();
    let mut cursor = 0;
    let mut ordinal = 1;
    while cursor < tree_output.stdout.len() {
        let remaining = &tree_output.stdout[cursor..];
        let record_end = match remaining.iter().position(|byte| *byte == 0) {
            Some(position) => position,
            None => {
                return Err(ScanError::MalformedTreeRecord {
                    ordinal,
                    reason: "missing NUL terminator",
                });
            }
        };
        let record = &remaining[..record_end];
        cursor += record_end + 1;
        if record.is_empty() {
            return Err(ScanError::MalformedTreeRecord {
                ordinal,
                reason: "empty record",
            });
        }

        let metadata_end = match record.iter().position(|byte| *byte == b'\t') {
            Some(position) => position,
            None => {
                return Err(ScanError::MalformedTreeRecord {
                    ordinal,
                    reason: "missing path separator",
                });
            }
        };
        let metadata = &record[..metadata_end];
        let path_bytes = &record[metadata_end + 1..];
        if path_bytes.is_empty() {
            return Err(ScanError::MalformedTreeRecord {
                ordinal,
                reason: "empty path",
            });
        }

        let mut fields = metadata.split(|byte| *byte == b' ');
        let mode = match fields.next() {
            Some(value) if !value.is_empty() => value,
            _ => {
                return Err(ScanError::MalformedTreeRecord {
                    ordinal,
                    reason: "missing mode",
                });
            }
        };
        let object_type = match fields.next() {
            Some(value) if !value.is_empty() => value,
            _ => {
                return Err(ScanError::MalformedTreeRecord {
                    ordinal,
                    reason: "missing object type",
                });
            }
        };
        let oid_bytes = match fields.next() {
            Some(value) if !value.is_empty() => value,
            _ => {
                return Err(ScanError::MalformedTreeRecord {
                    ordinal,
                    reason: "missing object id",
                });
            }
        };
        if fields.next().is_some() {
            return Err(ScanError::MalformedTreeRecord {
                ordinal,
                reason: "unexpected metadata field",
            });
        }

        if mode.len() != 6 || !mode.iter().all(|byte| matches!(*byte, b'0'..=b'7')) {
            return Err(ScanError::MalformedTreeRecord {
                ordinal,
                reason: "invalid mode",
            });
        }
        if object_type != b"blob" && object_type != b"commit" {
            return Err(ScanError::MalformedTreeRecord {
                ordinal,
                reason: "invalid object type",
            });
        }
        let valid_mode_type_pair = matches!(
            (mode, object_type),
            (b"100644" | b"100755" | b"120000", b"blob") | (b"160000", b"commit")
        );
        if !valid_mode_type_pair {
            return Err(ScanError::MalformedTreeRecord {
                ordinal,
                reason: "invalid mode and object type pair",
            });
        }

        if !is_valid_oid(oid_bytes) {
            return Err(ScanError::MalformedTreeRecord {
                ordinal,
                reason: "invalid object id",
            });
        }
        let mode = mode.iter().map(|byte| char::from(*byte)).collect();
        let oid = oid_bytes.iter().map(|byte| char::from(*byte)).collect();
        records.push(TreeRecord {
            mode,
            oid,
            path: path_bytes.to_vec(),
        });
        ordinal += 1;
    }

    if records.is_empty() {
        return Err(ScanError::EmptyHeadTree);
    }

    Ok(records)
}

/// Reads all blobs from validated committed-tree records without opening a
/// worktree path or resolving a symlink target. Gitlinks fail closed.
fn read_tree_blobs(
    repository_root: &Path,
    records: &[TreeRecord],
) -> Result<Vec<TrackedBlob>, ScanError> {
    for (index, record) in records.iter().enumerate() {
        if record.mode == "160000" {
            return Err(ScanError::UnsupportedTreeMode {
                ordinal: index + 1,
                mode: record.mode.clone(),
            });
        }
    }

    let mut request_file = match tempfile::tempfile() {
        Ok(file) => file,
        Err(error) => {
            return Err(ScanError::TempFile {
                operation: "creation",
                kind: error.kind(),
            });
        }
    };
    for record in records {
        if let Err(error) = request_file.write_all(record.oid.as_bytes()) {
            return Err(ScanError::TempFile {
                operation: "write",
                kind: error.kind(),
            });
        }
        if let Err(error) = request_file.write_all(b"\n") {
            return Err(ScanError::TempFile {
                operation: "write",
                kind: error.kind(),
            });
        }
    }
    if let Err(error) = request_file.flush() {
        return Err(ScanError::TempFile {
            operation: "flush",
            kind: error.kind(),
        });
    }
    if let Err(error) = request_file.seek(SeekFrom::Start(0)) {
        return Err(ScanError::TempFile {
            operation: "rewind",
            kind: error.kind(),
        });
    }

    let mut cat_file = Command::new("git");
    cat_file
        .args(["--no-replace-objects", "cat-file", "--batch", "--buffer"])
        .current_dir(repository_root)
        .stdin(Stdio::from(request_file));
    scrub_git_environment(&mut cat_file);
    let batch_output = match cat_file.output() {
        Ok(output) => output,
        Err(error) => {
            return Err(ScanError::GitSpawn {
                subcommand: "cat-file",
                kind: error.kind(),
            });
        }
    };
    if !batch_output.status.success() {
        return Err(ScanError::GitExit {
            subcommand: "cat-file",
            status: batch_output.status,
        });
    }

    let request_count = records.len();
    let mut blobs = Vec::with_capacity(request_count);
    let mut cursor = 0;
    for (index, record) in records.iter().enumerate() {
        let ordinal = index + 1;
        if cursor == batch_output.stdout.len() {
            return Err(ScanError::BatchResponseCount {
                expected: request_count,
                actual: index,
            });
        }

        let remaining = &batch_output.stdout[cursor..];
        let header_end = match remaining.iter().position(|byte| *byte == b'\n') {
            Some(position) => position,
            None => {
                return Err(ScanError::MalformedBatchResponse {
                    ordinal,
                    reason: "missing header terminator",
                });
            }
        };
        let header = &remaining[..header_end];
        cursor += header_end + 1;

        let rejection_reason = match header.strip_prefix(record.oid.as_bytes()) {
            Some(b" missing") => Some("missing object"),
            Some(b" ambiguous") => Some("ambiguous object"),
            _ => None,
        };
        if let Some(reason) = rejection_reason {
            return Err(ScanError::MalformedBatchResponse { ordinal, reason });
        }

        let mut fields = header.split(|byte| *byte == b' ');
        let actual_oid_bytes = match fields.next() {
            Some(value) if !value.is_empty() => value,
            _ => {
                return Err(ScanError::MalformedBatchResponse {
                    ordinal,
                    reason: "missing object id",
                });
            }
        };
        let object_type = match fields.next() {
            Some(value) if !value.is_empty() => value,
            _ => {
                return Err(ScanError::MalformedBatchResponse {
                    ordinal,
                    reason: "missing object type",
                });
            }
        };
        let size_bytes = match fields.next() {
            Some(value) if !value.is_empty() => value,
            _ => {
                return Err(ScanError::MalformedBatchResponse {
                    ordinal,
                    reason: "missing object size",
                });
            }
        };
        if fields.next().is_some() {
            return Err(ScanError::MalformedBatchResponse {
                ordinal,
                reason: "unexpected header field",
            });
        }

        if !is_valid_oid(actual_oid_bytes) {
            return Err(ScanError::MalformedBatchResponse {
                ordinal,
                reason: "invalid object id",
            });
        }
        let actual_oid = actual_oid_bytes
            .iter()
            .map(|byte| char::from(*byte))
            .collect();
        if actual_oid != record.oid {
            return Err(ScanError::BatchOidMismatch {
                ordinal,
                expected_oid: record.oid.clone(),
                actual_oid,
            });
        }
        if object_type != b"blob" {
            return Err(ScanError::BatchTypeMismatch {
                ordinal,
                oid: actual_oid,
            });
        }

        if !size_bytes.iter().all(u8::is_ascii_digit) {
            return Err(ScanError::MalformedBatchResponse {
                ordinal,
                reason: "invalid object size",
            });
        }
        let size_text: String = size_bytes.iter().map(|byte| char::from(*byte)).collect();
        let size = match size_text.parse::<usize>() {
            Ok(size) => size,
            Err(_) => {
                return Err(ScanError::MalformedBatchResponse {
                    ordinal,
                    reason: "object size overflow",
                });
            }
        };

        let payload_bytes = batch_output.stdout.len() - cursor;
        if payload_bytes < size {
            return Err(ScanError::MalformedBatchResponse {
                ordinal,
                reason: "truncated object payload",
            });
        }
        let payload_end = cursor + size;
        let bytes = batch_output.stdout[cursor..payload_end].to_vec();
        if batch_output.stdout.get(payload_end) != Some(&b'\n') {
            return Err(ScanError::MalformedBatchResponse {
                ordinal,
                reason: "invalid object terminator",
            });
        }
        cursor = payload_end + 1;
        blobs.push(TrackedBlob {
            path: record.path.clone(),
            bytes,
        });
    }
    if cursor != batch_output.stdout.len() {
        return Err(ScanError::TrailingBatchBytes {
            byte_count: batch_output.stdout.len() - cursor,
        });
    }
    Ok(blobs)
}

const SYNTHETIC_MARKER: &[u8] = b"synthetic-committed-tree-marker";
const SYNTHETIC_CLEAN_BLOB: &[u8] = b"synthetic clean blob contents";
const SYNTHETIC_REPLACEMENT_BLOB: &[u8] = b"synthetic replacement blob contents";
const SYNTHETIC_GITLINK_OID: &str = "1111111111111111111111111111111111111111";
static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

struct TestRepository {
    root: PathBuf,
}

impl TestRepository {
    fn new(label: &str) -> Self {
        let root = env::temp_dir().join(format!(
            "solstone-head-tree-test-{label}-{}-{}",
            std::process::id(),
            NEXT_TEMP.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).expect("create synthetic repository root");
        Self { root }
    }

    fn init(&self) {
        git_ok(&self.root, &["init", "-b", "main"]);
        git_ok(&self.root, &["config", "user.email", "tests@solstone.app"]);
        git_ok(&self.root, &["config", "user.name", "solstone tests"]);
        git_ok(&self.root, &["config", "core.autocrlf", "false"]);
    }

    fn commit(&self) {
        git_ok(
            &self.root,
            &["commit", "--no-gpg-sign", "-m", "synthetic committed tree"],
        );
    }

    fn write(&self, relative: &str, bytes: &[u8]) {
        let path = self.root.join(relative);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create synthetic file parent");
        }
        fs::write(path, bytes).expect("write synthetic repository file");
    }
}

impl Drop for TestRepository {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.root).expect("remove synthetic repository root");
    }
}

fn fixture_git_output(root: &Path, args: &[&str], stdin: Option<&[u8]>) -> std::process::Output {
    let mut git = Command::new("git");
    git.args(args).current_dir(root);
    scrub_git_environment(&mut git);
    if let Some(bytes) = stdin {
        let mut input = tempfile::tempfile().expect("create synthetic Git input");
        input.write_all(bytes).expect("write synthetic Git input");
        input.flush().expect("flush synthetic Git input");
        input
            .seek(SeekFrom::Start(0))
            .expect("rewind synthetic Git input");
        git.stdin(Stdio::from(input));
    }
    git.output().expect("run synthetic fixture Git")
}

fn git_ok(root: &Path, args: &[&str]) {
    let output = fixture_git_output(root, args, None);
    assert!(
        output.status.success(),
        "synthetic fixture Git exited with {}",
        output.status
    );
}

fn git_output_with_stdin(root: &Path, args: &[&str], stdin: &[u8]) -> Vec<u8> {
    let output = fixture_git_output(root, args, Some(stdin));
    assert!(
        output.status.success(),
        "synthetic fixture Git exited with {}",
        output.status
    );
    output.stdout
}

fn hash_blob(root: &Path, bytes: &[u8]) -> String {
    let output = git_output_with_stdin(root, &["hash-object", "-w", "--stdin"], bytes);
    String::from_utf8(output)
        .expect("synthetic object id must be UTF-8")
        .trim()
        .to_owned()
}

#[test]
fn head_tree_blobs_read_committed_file_when_worktree_file_is_missing() {
    let repository = TestRepository::new("deleted-worktree-file");
    repository.init();
    let path = "synthetic-deleted.txt";
    repository.write(path, SYNTHETIC_MARKER);
    git_ok(&repository.root, &["add", "--", path]);
    repository.commit();
    fs::remove_file(repository.root.join(path)).expect("delete synthetic worktree file");

    assert!(fs::read(repository.root.join(path)).is_err());
    let records = head_tree_records(&repository.root).expect("enumerate committed tree");
    let blobs = read_tree_blobs(&repository.root, &records).expect("read committed tree blobs");
    let blob = blobs
        .iter()
        .find(|blob| blob.path == path.as_bytes())
        .expect("find committed blob for missing worktree file");
    assert!(
        blob.path == path.as_bytes(),
        "unexpected committed blob path"
    );
    assert!(
        contains_subslice(&blob.bytes, SYNTHETIC_MARKER),
        "committed blob does not contain the synthetic marker"
    );
}

#[test]
fn head_tree_blobs_read_committed_broken_symlink_without_resolving_target() {
    let repository = TestRepository::new("broken-symlink");
    repository.init();
    let path = "synthetic-link";
    let oid = hash_blob(&repository.root, SYNTHETIC_MARKER);
    let cache_info = format!("120000,{oid},{path}");
    git_ok(
        &repository.root,
        &["update-index", "--add", "--cacheinfo", &cache_info],
    );
    repository.commit();

    assert!(
        fs::symlink_metadata(repository.root.join(path)).is_err(),
        "synthetic worktree symlink unexpectedly exists"
    );
    let records = head_tree_records(&repository.root).expect("enumerate committed tree");
    let blobs = read_tree_blobs(&repository.root, &records).expect("read committed tree blobs");
    let blob = blobs
        .iter()
        .find(|blob| blob.path == path.as_bytes())
        .expect("find committed broken-symlink blob");
    assert!(
        blob.path == path.as_bytes(),
        "unexpected committed symlink path"
    );
    assert!(
        contains_subslice(&blob.bytes, SYNTHETIC_MARKER),
        "committed symlink blob does not contain the synthetic marker"
    );
}

#[test]
fn head_tree_blobs_read_committed_file_after_staged_deletion() {
    let repository = TestRepository::new("staged-deletion");
    repository.init();
    let path = "synthetic-staged-deletion.txt";
    repository.write(path, SYNTHETIC_MARKER);
    git_ok(&repository.root, &["add", "--", path]);
    repository.commit();
    git_ok(&repository.root, &["rm", "--cached", "--", path]);

    let records = head_tree_records(&repository.root).expect("enumerate committed tree");
    let blobs = read_tree_blobs(&repository.root, &records).expect("read committed tree blobs");
    let blob = blobs
        .iter()
        .find(|blob| blob.path == path.as_bytes())
        .expect("find committed blob after staged deletion");
    assert!(
        contains_subslice(&blob.bytes, SYNTHETIC_MARKER),
        "committed blob does not contain the synthetic marker"
    );
}

#[test]
fn head_tree_records_expose_needle_bearing_filename() {
    let repository = TestRepository::new("needle-filename");
    repository.init();
    let marker = std::str::from_utf8(SYNTHETIC_MARKER).expect("synthetic marker must be UTF-8");
    let path = format!("prefix-{marker}-suffix.txt");
    repository.write(&path, SYNTHETIC_CLEAN_BLOB);
    git_ok(&repository.root, &["add", "--", &path]);
    repository.commit();

    let records = head_tree_records(&repository.root).expect("enumerate committed tree");
    let record = records
        .iter()
        .find(|record| contains_subslice(&record.path, SYNTHETIC_MARKER))
        .expect("find marker-bearing committed path");
    assert!(
        record.path == path.as_bytes(),
        "unexpected marker-bearing committed path"
    );
    let needles = vec![("marker", SYNTHETIC_MARKER.to_vec())];
    let scanned = [("path", record.path.as_slice(), record.path.as_slice())];
    let matches = collect_needle_matches(&scanned, &needles);
    assert!(
        matches.len() == 1 && matches[0].len() == 1,
        "marker-bearing path did not produce exactly one match"
    );
    assert!(matches[0][0].0 == "path", "marker hit was not labeled path");
    assert!(
        matches[0][0].1 == record.path,
        "marker hit did not retain its committed path"
    );
    let rendered = render_matches(&matches[0], &needles);
    assert!(
        rendered.len() == 1 && rendered[0] == "path: prefix-<redacted-needle>-suffix.txt",
        "marker-bearing path was not rendered in redacted form"
    );
    assert!(
        !contains_subslice(rendered[0].as_bytes(), SYNTHETIC_MARKER),
        "rendered path contains the synthetic marker"
    );
    let blobs = read_tree_blobs(&repository.root, &records).expect("read committed tree blobs");
    let blob = blobs
        .iter()
        .find(|blob| blob.path == path.as_bytes())
        .expect("find clean blob for marker-bearing path");
    assert!(
        !contains_subslice(&blob.bytes, SYNTHETIC_MARKER),
        "clean blob unexpectedly contains the synthetic marker"
    );
}

#[test]
fn head_tree_records_preserve_non_utf8_needle_bearing_path() {
    let repository = TestRepository::new("non-utf8-path");
    repository.init();
    let oid = hash_blob(&repository.root, SYNTHETIC_CLEAN_BLOB);
    let mut tree_info = format!("100644 {oid}\t").into_bytes();
    tree_info.extend_from_slice(b"raw-\xff-");
    tree_info.extend_from_slice(SYNTHETIC_MARKER);
    tree_info.push(b'\n');
    let output = git_output_with_stdin(
        &repository.root,
        &["update-index", "--index-info"],
        &tree_info,
    );
    assert!(output.is_empty(), "update-index returned unexpected output");
    repository.commit();

    let records = head_tree_records(&repository.root).expect("enumerate committed tree");
    let record = records
        .iter()
        .find(|record| contains_subslice(&record.path, SYNTHETIC_MARKER))
        .expect("find non-UTF-8 marker-bearing committed path");
    assert!(
        record.path.contains(&0xff),
        "committed path does not preserve the non-UTF-8 byte"
    );
    let needles = vec![("marker", SYNTHETIC_MARKER.to_vec())];
    let scanned = [("path", record.path.as_slice(), record.path.as_slice())];
    let matches = collect_needle_matches(&scanned, &needles);
    assert!(
        matches.len() == 1 && matches[0].len() == 1,
        "non-UTF-8 marker-bearing path did not produce exactly one match"
    );
    assert!(matches[0][0].0 == "path", "marker hit was not labeled path");
    let rendered = render_matches(&matches[0], &needles);
    assert!(
        rendered.len() == 1 && rendered[0].contains(REDACTED_NEEDLE),
        "non-UTF-8 path was not rendered with the redaction marker"
    );
    assert!(
        !contains_subslice(rendered[0].as_bytes(), SYNTHETIC_MARKER),
        "rendered non-UTF-8 path contains the synthetic marker"
    );
    let blobs = read_tree_blobs(&repository.root, &records).expect("read committed tree blobs");
    let blob = blobs
        .iter()
        .find(|blob| blob.path == record.path)
        .expect("find blob for non-UTF-8 committed path");
    assert!(
        !contains_subslice(&blob.bytes, SYNTHETIC_MARKER),
        "clean blob unexpectedly contains the synthetic marker"
    );
}

#[test]
fn head_tree_blobs_read_original_bytes_despite_an_active_replace_ref() {
    let repository = TestRepository::new("replace-ref");
    repository.init();
    let path = "synthetic-replaced.txt";
    let original_oid = hash_blob(&repository.root, SYNTHETIC_MARKER);
    let cache_info = format!("100644,{original_oid},{path}");
    git_ok(
        &repository.root,
        &["update-index", "--add", "--cacheinfo", &cache_info],
    );
    repository.commit();
    let replacement_oid = hash_blob(&repository.root, SYNTHETIC_REPLACEMENT_BLOB);
    git_ok(
        &repository.root,
        &["replace", "-f", &original_oid, &replacement_oid],
    );

    let records = head_tree_records(&repository.root).expect("enumerate committed tree");
    let blobs = read_tree_blobs(&repository.root, &records).expect("read committed tree blobs");
    let blob = blobs
        .iter()
        .find(|blob| blob.path == path.as_bytes())
        .expect("find committed blob with active replacement");
    assert!(
        blob.bytes == SYNTHETIC_MARKER,
        "batch read did not return the original committed bytes"
    );
    assert!(
        blob.bytes != SYNTHETIC_REPLACEMENT_BLOB,
        "batch read returned replacement bytes"
    );
}

#[test]
fn head_tree_records_expose_gitlink_path_before_blob_read_rejects_mode() {
    let repository = TestRepository::new("gitlink-mode");
    repository.init();
    let marker = std::str::from_utf8(SYNTHETIC_MARKER).expect("synthetic marker must be UTF-8");
    let path = format!("synthetic-gitlink-{marker}");
    let cache_info = format!("160000,{SYNTHETIC_GITLINK_OID},{path}");
    git_ok(
        &repository.root,
        &["update-index", "--add", "--cacheinfo", &cache_info],
    );
    repository.commit();

    let records = head_tree_records(&repository.root).expect("enumerate committed tree");
    let record = records
        .iter()
        .find(|record| record.path == path.as_bytes())
        .expect("find committed gitlink path");
    assert!(record.mode == "160000", "unexpected committed gitlink mode");
    let needles = vec![("marker", SYNTHETIC_MARKER.to_vec())];
    let scanned = [("path", record.path.as_slice(), record.path.as_slice())];
    let matches = collect_needle_matches(&scanned, &needles);
    assert!(
        matches.len() == 1 && matches[0].len() == 1,
        "marker-bearing gitlink path did not produce exactly one match"
    );
    assert!(
        matches[0][0].0 == "path",
        "gitlink marker hit was not labeled path"
    );
    match read_tree_blobs(&repository.root, &records) {
        Err(ScanError::UnsupportedTreeMode { mode, .. }) => {
            assert!(mode == "160000", "unexpected unsupported tree mode");
        }
        Err(error) => panic!("unexpected synthetic scan error: {error}"),
        Ok(blobs) => panic!(
            "synthetic gitlink scan unexpectedly returned {} blobs",
            blobs.len()
        ),
    }
}

#[test]
fn head_tree_records_report_unborn_head() {
    let repository = TestRepository::new("unborn-head");
    repository.init();

    match head_tree_records(&repository.root) {
        Err(ScanError::UnbornHead) => {}
        Err(error) => panic!("unexpected synthetic scan error: {error}"),
        Ok(records) => panic!(
            "synthetic unborn-HEAD scan unexpectedly returned {} records",
            records.len()
        ),
    }
}

#[test]
fn head_tree_records_reject_empty_committed_tree() {
    let repository = TestRepository::new("empty-tree");
    repository.init();
    git_ok(
        &repository.root,
        &[
            "commit",
            "--allow-empty",
            "--no-gpg-sign",
            "-m",
            "synthetic empty tree",
        ],
    );

    match head_tree_records(&repository.root) {
        Err(ScanError::EmptyHeadTree) => {}
        Err(error) => panic!("unexpected synthetic scan error: {error}"),
        Ok(records) => panic!(
            "synthetic empty-tree scan unexpectedly returned {} records",
            records.len()
        ),
    }
}

#[test]
fn head_tree_records_return_git_exit_outside_a_repository() {
    let repository = TestRepository::new("not-a-repository");
    match head_tree_records(&repository.root) {
        Err(ScanError::GitExit { subcommand, .. }) => {
            assert!(
                subcommand == "rev-parse",
                "unexpected failing Git subcommand"
            );
        }
        Err(error) => panic!("unexpected synthetic scan error: {error}"),
        Ok(records) => panic!(
            "synthetic non-repository scan unexpectedly returned {} records",
            records.len()
        ),
    }
}
