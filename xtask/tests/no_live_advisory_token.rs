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
    let blobs = match tracked_blobs(workspace_root) {
        Ok(blobs) => blobs,
        Err(error) => panic!("tracked-tree needle scan error: {error}"),
    };

    let mut matches = vec![Vec::new(); needles.len()];
    for tracked_blob in &blobs {
        for (index, (_, needle)) in needles.iter().enumerate() {
            if contains_subslice(&tracked_blob.bytes, needle) {
                matches[index].push(tracked_blob.path.as_str());
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
            "tracked-tree needle match found ({label}): {} offending tracked paths:\n{}",
            offending_paths.len(),
            redacted_paths.join("\n")
        );
    }
}

#[derive(Debug)]
struct TrackedBlob {
    path: String,
    bytes: Vec<u8>,
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
    MalformedIndexRecord {
        ordinal: usize,
        reason: &'static str,
    },
    UnsupportedIndexMode {
        ordinal: usize,
        mode: String,
    },
    UnsupportedIndexStage {
        ordinal: usize,
        stage: String,
    },
    NonUtf8IndexPath {
        ordinal: usize,
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
            Self::MalformedIndexRecord { ordinal, reason } => {
                write!(formatter, "index record {ordinal} is malformed ({reason})")
            }
            Self::UnsupportedIndexMode { ordinal, mode } => {
                write!(formatter, "index record {ordinal} has unsupported mode {mode}")
            }
            Self::UnsupportedIndexStage { ordinal, stage } => write!(
                formatter,
                "index record {ordinal} has unsupported stage {stage}"
            ),
            Self::NonUtf8IndexPath { ordinal } => {
                write!(formatter, "index record {ordinal} has a non-UTF-8 path")
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

const GIT_SELECTION_ENV_VARS: [&str; 7] = [
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_INDEX_FILE",
    "GIT_COMMON_DIR",
    "GIT_OBJECT_DIRECTORY",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_NAMESPACE",
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

/// Returns indexed bytes for every stage-zero regular, executable, and symlink
/// blob. It deliberately does not read or require a clean worktree; unstaged
/// and worktree-only changes are outside this tracked-index scan.
fn tracked_blobs(repository_root: &Path) -> Result<Vec<TrackedBlob>, ScanError> {
    let is_valid_oid = |value: &[u8]| {
        matches!(value.len(), 40 | 64)
            && value
                .iter()
                .all(|byte| matches!(*byte, b'0'..=b'9' | b'a'..=b'f'))
    };

    let mut ls_files = Command::new("git");
    ls_files
        .args(["ls-files", "-s", "-z"])
        .current_dir(repository_root);
    scrub_git_environment(&mut ls_files);
    let index_output = match ls_files.output() {
        Ok(output) => output,
        Err(error) => {
            return Err(ScanError::GitSpawn {
                subcommand: "ls-files",
                kind: error.kind(),
            });
        }
    };
    if !index_output.status.success() {
        return Err(ScanError::GitExit {
            subcommand: "ls-files",
            status: index_output.status,
        });
    }

    let mut entries = Vec::new();
    let mut cursor = 0;
    let mut ordinal = 1;
    while cursor < index_output.stdout.len() {
        let remaining = &index_output.stdout[cursor..];
        let record_end = match remaining.iter().position(|byte| *byte == 0) {
            Some(position) => position,
            None => {
                return Err(ScanError::MalformedIndexRecord {
                    ordinal,
                    reason: "missing NUL terminator",
                });
            }
        };
        let record = &remaining[..record_end];
        cursor += record_end + 1;
        if record.is_empty() {
            return Err(ScanError::MalformedIndexRecord {
                ordinal,
                reason: "empty record",
            });
        }

        let metadata_end = match record.iter().position(|byte| *byte == b'\t') {
            Some(position) => position,
            None => {
                return Err(ScanError::MalformedIndexRecord {
                    ordinal,
                    reason: "missing metadata separator",
                });
            }
        };
        let metadata = &record[..metadata_end];
        let path_bytes = &record[metadata_end + 1..];
        if path_bytes.is_empty() {
            return Err(ScanError::MalformedIndexRecord {
                ordinal,
                reason: "empty path",
            });
        }

        let mut fields = metadata.split(|byte| *byte == b' ');
        let mode = match fields.next() {
            Some(value) if !value.is_empty() => value,
            _ => {
                return Err(ScanError::MalformedIndexRecord {
                    ordinal,
                    reason: "missing mode",
                });
            }
        };
        let oid_bytes = match fields.next() {
            Some(value) if !value.is_empty() => value,
            _ => {
                return Err(ScanError::MalformedIndexRecord {
                    ordinal,
                    reason: "missing object id",
                });
            }
        };
        let stage = match fields.next() {
            Some(value) if !value.is_empty() => value,
            _ => {
                return Err(ScanError::MalformedIndexRecord {
                    ordinal,
                    reason: "missing stage",
                });
            }
        };
        if fields.next().is_some() {
            return Err(ScanError::MalformedIndexRecord {
                ordinal,
                reason: "unexpected metadata field",
            });
        }

        if mode.len() != 6 || !mode.iter().all(|byte| matches!(*byte, b'0'..=b'7')) {
            return Err(ScanError::MalformedIndexRecord {
                ordinal,
                reason: "invalid mode",
            });
        }
        if mode != b"100644" && mode != b"100755" && mode != b"120000" {
            let mode = mode.iter().map(|byte| char::from(*byte)).collect();
            return Err(ScanError::UnsupportedIndexMode { ordinal, mode });
        }

        if !is_valid_oid(oid_bytes) {
            return Err(ScanError::MalformedIndexRecord {
                ordinal,
                reason: "invalid object id",
            });
        }
        let oid: String = oid_bytes.iter().map(|byte| char::from(*byte)).collect();

        if !stage.iter().all(u8::is_ascii_digit) {
            return Err(ScanError::MalformedIndexRecord {
                ordinal,
                reason: "invalid stage",
            });
        }
        if stage != b"0" {
            let stage = stage.iter().map(|byte| char::from(*byte)).collect();
            return Err(ScanError::UnsupportedIndexStage { ordinal, stage });
        }

        let path = match String::from_utf8(path_bytes.to_vec()) {
            Ok(path) => path,
            Err(_) => {
                return Err(ScanError::NonUtf8IndexPath { ordinal });
            }
        };
        entries.push((path, oid));
        ordinal += 1;
    }

    if entries.is_empty() {
        return Ok(Vec::new());
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
    for (_, oid) in &entries {
        if let Err(error) = request_file.write_all(oid.as_bytes()) {
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
        .args(["cat-file", "--batch", "--buffer"])
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

    let request_count = entries.len();
    let mut blobs = Vec::with_capacity(request_count);
    let mut cursor = 0;
    for (index, (path, expected_oid)) in entries.into_iter().enumerate() {
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

        let rejection_reason = match header.strip_prefix(expected_oid.as_bytes()) {
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
        if actual_oid != expected_oid {
            return Err(ScanError::BatchOidMismatch {
                ordinal,
                expected_oid,
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
        blobs.push(TrackedBlob { path, bytes });
    }
    if cursor != batch_output.stdout.len() {
        return Err(ScanError::TrailingBatchBytes {
            byte_count: batch_output.stdout.len() - cursor,
        });
    }
    Ok(blobs)
}

const SYNTHETIC_MARKER: &[u8] = b"synthetic-index-only-blob";
const SYNTHETIC_GITLINK_OID: &str = "1111111111111111111111111111111111111111";
static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

struct TestRepository {
    root: PathBuf,
}

impl TestRepository {
    fn new(label: &str) -> Self {
        let root = env::temp_dir().join(format!(
            "solstone-index-blob-test-{label}-{}-{}",
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
        "synthetic fixture Git failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn git_output_with_stdin(root: &Path, args: &[&str], stdin: &[u8]) -> Vec<u8> {
    let output = fixture_git_output(root, args, Some(stdin));
    assert!(
        output.status.success(),
        "synthetic fixture Git failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}

fn hash_synthetic_marker(root: &Path) -> String {
    let output = git_output_with_stdin(root, &["hash-object", "-w", "--stdin"], SYNTHETIC_MARKER);
    String::from_utf8(output)
        .expect("synthetic object id must be UTF-8")
        .trim()
        .to_owned()
}

#[test]
fn tracked_blobs_reads_deleted_worktree_file_from_index() {
    let repository = TestRepository::new("deleted-worktree-file");
    repository.init();
    let path = "synthetic-deleted.txt";
    repository.write(path, SYNTHETIC_MARKER);
    git_ok(&repository.root, &["add", "--", path]);
    fs::remove_file(repository.root.join(path)).expect("delete synthetic worktree file");

    assert!(fs::read(repository.root.join(path)).is_err());
    let blobs = tracked_blobs(&repository.root).expect("read indexed synthetic blob");
    let blob = blobs
        .iter()
        .find(|blob| blob.path == path)
        .expect("find deleted-worktree indexed blob");
    assert_eq!(blob.path.as_str(), path);
    assert!(contains_subslice(&blob.bytes, SYNTHETIC_MARKER));
}

#[test]
fn tracked_blobs_reads_staged_symlink_blob_without_worktree_symlink() {
    let repository = TestRepository::new("staged-symlink");
    repository.init();
    let path = "synthetic-link";
    let oid = hash_synthetic_marker(&repository.root);
    let cache_info = format!("120000,{oid},{path}");
    git_ok(
        &repository.root,
        &["update-index", "--add", "--cacheinfo", &cache_info],
    );

    assert!(fs::read(repository.root.join(path)).is_err());
    let blobs = tracked_blobs(&repository.root).expect("read indexed synthetic symlink");
    let blob = blobs
        .iter()
        .find(|blob| blob.path == path)
        .expect("find no-worktree symlink blob");
    assert_eq!(blob.path.as_str(), path);
    assert!(contains_subslice(&blob.bytes, SYNTHETIC_MARKER));
}

#[test]
fn tracked_blobs_returns_error_outside_a_repository() {
    let repository = TestRepository::new("not-a-repository");
    match tracked_blobs(&repository.root) {
        Err(ScanError::GitExit { subcommand, .. }) => assert_eq!(subcommand, "ls-files"),
        Err(error) => panic!("unexpected synthetic scan error: {error}"),
        Ok(blobs) => panic!(
            "synthetic non-repository scan unexpectedly returned {} blobs",
            blobs.len()
        ),
    }
}

#[test]
fn tracked_blobs_rejects_gitlink_mode() {
    let repository = TestRepository::new("gitlink-mode");
    repository.init();
    let path = "synthetic-gitlink";
    let cache_info = format!("160000,{SYNTHETIC_GITLINK_OID},{path}");
    git_ok(
        &repository.root,
        &["update-index", "--add", "--cacheinfo", &cache_info],
    );

    match tracked_blobs(&repository.root) {
        Err(ScanError::UnsupportedIndexMode { mode, .. }) => assert_eq!(mode, "160000"),
        Err(error) => panic!("unexpected synthetic scan error: {error}"),
        Ok(blobs) => panic!(
            "synthetic gitlink scan unexpectedly returned {} blobs",
            blobs.len()
        ),
    }
}

#[test]
fn tracked_blobs_rejects_nonzero_index_stage() {
    let repository = TestRepository::new("nonzero-stage");
    repository.init();
    let path = "synthetic-unmerged.txt";
    let oid = hash_synthetic_marker(&repository.root);
    let index_info = format!("100644 {oid} 1\t{path}\n");
    let output = git_output_with_stdin(
        &repository.root,
        &["update-index", "--index-info"],
        index_info.as_bytes(),
    );
    assert!(output.is_empty());

    match tracked_blobs(&repository.root) {
        Err(ScanError::UnsupportedIndexStage { stage, .. }) => assert_eq!(stage, "1"),
        Err(error) => panic!("unexpected synthetic scan error: {error}"),
        Ok(blobs) => panic!(
            "synthetic nonzero-stage scan unexpectedly returned {} blobs",
            blobs.len()
        ),
    }
}
