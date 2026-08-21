// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Parsing and validation for `--integration <operation>`.
//!
//! Pure: it never touches the filesystem or the network, so the whole surface —
//! including every rejection — is unit-testable in a crate a gate actually
//! compiles. Values are never echoed back into an error; only our own flag names
//! appear, because a value can be a path, a link, or a secret.

use std::path::PathBuf;
use std::time::Duration;

use observer_pl::mux::INITIAL_WINDOW;

/// The flag that selects the mode.
pub const MODE_FLAG: &str = "--integration";

/// An upper bound on `--deadline-secs`, so a typo cannot wedge an operator run.
const MAX_DEADLINE_SECS: u64 = 3_600;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Operation {
    Pair,
    Roundtrip,
    Fetch,
    Upload,
}

impl Operation {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pair => "pair",
            Self::Roundtrip => "roundtrip",
            Self::Fetch => "fetch",
            Self::Upload => "upload",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "pair" => Some(Self::Pair),
            "roundtrip" => Some(Self::Roundtrip),
            "fetch" => Some(Self::Fetch),
            "upload" => Some(Self::Upload),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OperationArgs {
    Pair,
    Roundtrip,
    Fetch {
        journal_path: String,
        expected_bytes: u64,
        expected_sha256: String,
        expected_status: u16,
    },
    Upload {
        payload: PathBuf,
        day: String,
        segment: String,
        carrier: Carrier,
    },
}

/// The carrier an operator requires the upload gate to observe.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Carrier {
    Direct,
    Relay,
}

impl Carrier {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Direct => "direct",
            Self::Relay => "relay",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Command {
    pub operation: Operation,
    pub deadline: Duration,
    pub max_dials: Option<u64>,
    pub args: OperationArgs,
}

/// A rejection carrying a stable token and guidance naming only our own flags.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArgError {
    pub reason: &'static str,
    pub guidance: String,
    /// The operation name for the envelope when it could be determined.
    pub operation: &'static str,
}

impl ArgError {
    fn new(reason: &'static str, operation: &'static str, guidance: impl Into<String>) -> Self {
        Self {
            reason,
            guidance: guidance.into(),
            operation,
        }
    }
}

/// True when the process args select the integration mode at all.
///
/// This lives in the gated crate so both the positive dispatch and the
/// "a bare launch is untouched" negative are covered by a test a gate runs.
pub fn is_selected<S: AsRef<str>>(args: &[S]) -> bool {
    args.iter().any(|arg| arg.as_ref() == MODE_FLAG)
}

fn value_after<'a, S: AsRef<str>>(args: &'a [S], flag: &str) -> Option<&'a str> {
    let index = args.iter().position(|arg| arg.as_ref() == flag)?;
    args.get(index + 1).map(|value| value.as_ref())
}

fn required<'a, S: AsRef<str>>(
    args: &'a [S],
    flag: &'static str,
    operation: &'static str,
) -> Result<&'a str, ArgError> {
    value_after(args, flag).ok_or_else(|| {
        ArgError::new(
            "arg_missing",
            operation,
            format!("{operation} requires {flag}"),
        )
    })
}

fn parse_u64(value: &str, flag: &'static str, operation: &'static str) -> Result<u64, ArgError> {
    value.parse::<u64>().map_err(|_| {
        ArgError::new(
            "arg_bad_value",
            operation,
            format!("{flag} must be a non-negative whole number"),
        )
    })
}

/// Lowercase hex of exactly `bytes` bytes. Uppercase is rejected rather than
/// normalized, so an expectation always compares byte-for-byte against the
/// digests this codebase emits.
fn parse_lower_hex(
    value: &str,
    bytes: usize,
    flag: &'static str,
    operation: &'static str,
) -> Result<String, ArgError> {
    let expected = bytes * 2;
    let valid = value.len() == expected
        && value
            .chars()
            .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c));
    if !valid {
        return Err(ArgError::new(
            "arg_bad_value",
            operation,
            format!("{flag} must be {expected} lowercase hex characters"),
        ));
    }
    Ok(value.to_string())
}

/// `YYYYMMDD`, digits only — the journal's `day` shape.
fn parse_day(value: &str, operation: &'static str) -> Result<String, ArgError> {
    if value.len() == 8 && value.chars().all(|c| c.is_ascii_digit()) {
        Ok(value.to_string())
    } else {
        Err(ArgError::new(
            "arg_bad_value",
            operation,
            "--day must be YYYYMMDD",
        ))
    }
}

/// `HHMMSS_LEN` — the journal's `segment` shape. The civil validity of the time
/// and the round-trip against production key derivation are checked later, by
/// the uploader path itself.
fn parse_segment(value: &str, operation: &'static str) -> Result<String, ArgError> {
    let bad = || {
        ArgError::new(
            "arg_bad_value",
            operation,
            "--segment must be HHMMSS_LEN, e.g. 143000_300",
        )
    };
    let (time, len) = value.split_once('_').ok_or_else(bad)?;
    if time.len() != 6 || !time.chars().all(|c| c.is_ascii_digit()) {
        return Err(bad());
    }
    if len.is_empty() || !len.chars().all(|c| c.is_ascii_digit()) {
        return Err(bad());
    }
    if len.parse::<u64>().map_err(|_| bad())? == 0 {
        return Err(bad());
    }
    Ok(value.to_string())
}

fn parse_carrier<S: AsRef<str>>(args: &[S], operation: &'static str) -> Result<Carrier, ArgError> {
    let Some(value) = value_after(args, "--carrier") else {
        return Err(ArgError::new(
            "carrier_missing",
            operation,
            "upload requires --carrier direct or --carrier relay",
        ));
    };
    match value {
        "direct" => Ok(Carrier::Direct),
        "relay" => Ok(Carrier::Relay),
        _ => Err(ArgError::new(
            "carrier_invalid",
            operation,
            "--carrier accepts exactly: direct, relay",
        )),
    }
}

/// Parse the whole mode invocation. `args` is the process argument list with the
/// executable already stripped.
pub fn parse<S: AsRef<str>>(args: &[S]) -> Result<Command, ArgError> {
    let operation_value = value_after(args, MODE_FLAG).ok_or_else(|| {
        ArgError::new(
            "operation_missing",
            "unknown",
            "--integration requires an operation: pair, roundtrip, fetch, or upload",
        )
    })?;
    let operation = Operation::parse(operation_value).ok_or_else(|| {
        // The rejected value is never echoed: it is caller-supplied text.
        ArgError::new(
            "operation_unknown",
            "unknown",
            "--integration accepts exactly: pair, roundtrip, fetch, upload",
        )
    })?;
    let name = operation.as_str();

    let deadline_secs = parse_u64(
        required(args, "--deadline-secs", name)?,
        "--deadline-secs",
        name,
    )?;
    if deadline_secs == 0 || deadline_secs > MAX_DEADLINE_SECS {
        return Err(ArgError::new(
            "arg_bad_value",
            name,
            format!("--deadline-secs must be between 1 and {MAX_DEADLINE_SECS}"),
        ));
    }

    let max_dials = match value_after(args, "--max-dials") {
        Some(value) => {
            let parsed = parse_u64(value, "--max-dials", name)?;
            if parsed == 0 {
                return Err(ArgError::new(
                    "arg_bad_value",
                    name,
                    "--max-dials must be at least 1",
                ));
            }
            Some(parsed)
        }
        None => None,
    };

    let args_for_operation = match operation {
        Operation::Pair => OperationArgs::Pair,
        Operation::Roundtrip => OperationArgs::Roundtrip,
        Operation::Fetch => {
            let journal_path = required(args, "--journal-path", name)?.to_string();
            if !journal_path.starts_with('/') {
                return Err(ArgError::new(
                    "arg_bad_value",
                    name,
                    "--journal-path must be an absolute journal path beginning with /",
                ));
            }
            let expected_bytes = parse_u64(
                required(args, "--expected-bytes", name)?,
                "--expected-bytes",
                name,
            )?;
            // AC4's precondition, enforced before anything touches the network.
            if expected_bytes as usize <= INITIAL_WINDOW {
                return Err(ArgError::new(
                    "expected_bytes_not_over_window",
                    name,
                    format!(
                        "--expected-bytes must be strictly greater than the protocol initial receive window of {INITIAL_WINDOW} bytes, so the fetch exercises flow control"
                    ),
                ));
            }
            let expected_sha256 = parse_lower_hex(
                required(args, "--expected-sha256", name)?,
                32,
                "--expected-sha256",
                name,
            )?;
            let expected_status = parse_u64(
                required(args, "--expected-status", name)?,
                "--expected-status",
                name,
            )?;
            let expected_status = u16::try_from(expected_status)
                .ok()
                .filter(|status| (100..=599).contains(status))
                .ok_or_else(|| {
                    ArgError::new(
                        "arg_bad_value",
                        name,
                        "--expected-status must be a valid HTTP status between 100 and 599",
                    )
                })?;
            OperationArgs::Fetch {
                journal_path,
                expected_bytes,
                expected_sha256,
                expected_status,
            }
        }
        Operation::Upload => OperationArgs::Upload {
            payload: PathBuf::from(required(args, "--payload", name)?),
            day: parse_day(required(args, "--day", name)?, name)?,
            segment: parse_segment(required(args, "--segment", name)?, name)?,
            carrier: parse_carrier(args, name)?,
        },
    };

    Ok(Command {
        operation,
        deadline: Duration::from_secs(deadline_secs),
        max_dials,
        args: args_for_operation,
    })
}

/// Operator help, written to stderr. Operator-facing, so the engineering
/// vocabulary is correct here.
pub const HELP: &str = "\
--integration <operation> — drive the app's production transport against a journal.

Operations:
  pair        pair and register from one relay-form link on stdin (requires an empty profile)
  roundtrip   authenticated heartbeat + segment list over the production client
  fetch       retrieve a journal path through the production journal bridge
  upload      send one caller-named synthetic segment through the production uploader

Common flags:
  --deadline-secs <n>      required; one budget for awaited async work; blocking
                           local stdin/file reads or writes, hashing, and envelope
                           serialization stay outside it; use the caller's process
                           timeout to bound them
  --max-dials <n>          optional; exceeding it is an assertion failure, not a pass

fetch flags:
  --journal-path <path>    absolute journal path to retrieve
  --expected-bytes <n>     must exceed the protocol initial receive window
  --expected-sha256 <hex>  64 lowercase hex characters
  --expected-status <n>    expected HTTP status

upload flags:
  --payload <path>         file to send as the segment's only member
  --day <YYYYMMDD>         caller-named day
  --segment <HHMMSS_LEN>   caller-named segment key
  --carrier <direct|relay> required carrier the successful custody witness must show

Exactly one JSON object is written to stdout; progress goes to stderr.
Exit: 0 pass, 1 assertion failed, 2 error, 3 deadline exceeded.";

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    fn over_window() -> String {
        (INITIAL_WINDOW as u64 + 1).to_string()
    }

    const SHA: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    #[test]
    fn selection_requires_the_mode_flag() {
        assert!(is_selected(&args(&["--integration", "pair"])));
        assert!(!is_selected::<String>(&[]));
        assert!(!is_selected(&args(&["--open-view", "settings"])));
        assert!(!is_selected(&args(&["--dump-state"])));
        // A bare GUI launch must never be mistaken for the mode.
        assert!(!is_selected(&args(&["--from-autostart"])));
    }

    #[test]
    fn parses_a_minimal_roundtrip() {
        let command = parse(&args(&[
            "--integration",
            "roundtrip",
            "--deadline-secs",
            "30",
        ]))
        .unwrap();
        assert_eq!(command.operation, Operation::Roundtrip);
        assert_eq!(command.deadline, Duration::from_secs(30));
        assert_eq!(command.max_dials, None);
        assert_eq!(command.args, OperationArgs::Roundtrip);
    }

    #[test]
    fn missing_operation_is_a_named_rejection_not_a_panic() {
        let error = parse(&args(&["--integration"])).unwrap_err();
        assert_eq!(error.reason, "operation_missing");
        assert_eq!(error.operation, "unknown");
    }

    #[test]
    fn unknown_operation_never_echoes_the_caller_value() {
        let error = parse(&args(&["--integration", "../../etc/passwd"])).unwrap_err();
        assert_eq!(error.reason, "operation_unknown");
        assert!(!error.guidance.contains("passwd"));
    }

    #[test]
    fn deadline_is_required_and_bounded() {
        let missing = parse(&args(&["--integration", "pair"])).unwrap_err();
        assert_eq!(missing.reason, "arg_missing");

        for bad in ["0", "3601", "-1", "abc", ""] {
            let error =
                parse(&args(&["--integration", "pair", "--deadline-secs", bad])).unwrap_err();
            assert_eq!(error.reason, "arg_bad_value", "value {bad:?} should reject");
        }
    }

    #[test]
    fn fetch_requires_expected_bytes_over_the_initial_window() {
        for at_or_under in [0u64, 1, INITIAL_WINDOW as u64] {
            let error = parse(&args(&[
                "--integration",
                "fetch",
                "--deadline-secs",
                "30",
                "--journal-path",
                "/day/20260617",
                "--expected-bytes",
                &at_or_under.to_string(),
                "--expected-sha256",
                SHA,
                "--expected-status",
                "200",
            ]))
            .unwrap_err();
            assert_eq!(error.reason, "expected_bytes_not_over_window");
        }

        let ok = parse(&args(&[
            "--integration",
            "fetch",
            "--deadline-secs",
            "30",
            "--journal-path",
            "/day/20260617",
            "--expected-bytes",
            &over_window(),
            "--expected-sha256",
            SHA,
            "--expected-status",
            "200",
        ]))
        .unwrap();
        assert!(matches!(ok.args, OperationArgs::Fetch { .. }));
    }

    #[test]
    fn fetch_rejects_a_relative_path_and_a_bad_digest() {
        let relative = parse(&args(&[
            "--integration",
            "fetch",
            "--deadline-secs",
            "30",
            "--journal-path",
            "day/20260617",
            "--expected-bytes",
            &over_window(),
            "--expected-sha256",
            SHA,
            "--expected-status",
            "200",
        ]))
        .unwrap_err();
        assert_eq!(relative.reason, "arg_bad_value");

        for bad_sha in [
            "short",
            &SHA.to_uppercase(),
            &format!("{SHA}0"),
            &"g".repeat(64),
        ] {
            let error = parse(&args(&[
                "--integration",
                "fetch",
                "--deadline-secs",
                "30",
                "--journal-path",
                "/x",
                "--expected-bytes",
                &over_window(),
                "--expected-sha256",
                bad_sha,
                "--expected-status",
                "200",
            ]))
            .unwrap_err();
            assert_eq!(error.reason, "arg_bad_value", "sha {bad_sha:?}");
        }
    }

    #[test]
    fn fetch_rejects_an_impossible_status() {
        for bad in ["0", "99", "600", "70000"] {
            let error = parse(&args(&[
                "--integration",
                "fetch",
                "--deadline-secs",
                "30",
                "--journal-path",
                "/x",
                "--expected-bytes",
                &over_window(),
                "--expected-sha256",
                SHA,
                "--expected-status",
                bad,
            ]))
            .unwrap_err();
            assert_eq!(error.reason, "arg_bad_value", "status {bad}");
        }
    }

    #[test]
    fn upload_validates_the_day_and_segment_shapes() {
        let good = parse(&args(&[
            "--integration",
            "upload",
            "--deadline-secs",
            "60",
            "--payload",
            "/tmp/x.bin",
            "--day",
            "20260617",
            "--segment",
            "143000_300",
            "--carrier",
            "direct",
        ]))
        .unwrap();
        assert_eq!(
            good.args,
            OperationArgs::Upload {
                payload: PathBuf::from("/tmp/x.bin"),
                day: "20260617".to_string(),
                segment: "143000_300".to_string(),
                carrier: Carrier::Direct,
            }
        );

        for (day, segment) in [
            ("2026061", "143000_300"),
            ("2026-06-17", "143000_300"),
            ("20260617", "143000"),
            ("20260617", "14300_300"),
            ("20260617", "143000_"),
            ("20260617", "143000_0"),
            ("20260617", "abcdef_300"),
        ] {
            let error = parse(&args(&[
                "--integration",
                "upload",
                "--deadline-secs",
                "60",
                "--payload",
                "/tmp/x.bin",
                "--day",
                day,
                "--segment",
                segment,
                "--carrier",
                "relay",
            ]))
            .unwrap_err();
            assert_eq!(error.reason, "arg_bad_value", "{day} {segment}");
        }
    }

    #[test]
    fn upload_requires_a_named_valid_carrier() {
        let base = [
            "--integration",
            "upload",
            "--deadline-secs",
            "60",
            "--payload",
            "/var/tmp/payload.bin",
            "--day",
            "20260617",
            "--segment",
            "143000_300",
        ];
        assert_eq!(parse(&args(&base)).unwrap_err().reason, "carrier_missing");
        let mut invalid = base.to_vec();
        invalid.extend(["--carrier", "lan"]);
        assert_eq!(
            parse(&args(&invalid)).unwrap_err().reason,
            "carrier_invalid"
        );
        assert!(HELP.contains("--carrier <direct|relay>"));
    }

    #[test]
    fn max_dials_is_optional_but_must_be_positive() {
        let with = parse(&args(&[
            "--integration",
            "roundtrip",
            "--deadline-secs",
            "30",
            "--max-dials",
            "4",
        ]))
        .unwrap();
        assert_eq!(with.max_dials, Some(4));

        let error = parse(&args(&[
            "--integration",
            "roundtrip",
            "--deadline-secs",
            "30",
            "--max-dials",
            "0",
        ]))
        .unwrap_err();
        assert_eq!(error.reason, "arg_bad_value");
    }

    #[test]
    fn help_names_every_operation_and_exit_status() {
        for token in ["pair", "roundtrip", "fetch", "upload"] {
            assert!(HELP.contains(token));
        }
        for token in ["--deadline-secs", "--max-dials", "stdout", "stderr"] {
            assert!(HELP.contains(token));
        }
    }
}
