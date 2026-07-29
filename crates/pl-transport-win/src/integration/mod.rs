// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! The artifact-native operator integration mode.
//!
//! `--integration <operation>` drives the app's **production** pairing, relay,
//! mTLS, framing, observer client, journal bridge, and uploader against a
//! caller-provided journal, writes exactly one schema-versioned JSON object to
//! stdout, and fails closed. It composes and observes production behavior; it
//! never substitutes it — there is no second transport, no mock route, and no
//! success override anywhere in this module.
//!
//! Everything testable lives here rather than in the app crate, because no
//! repository gate compiles `src-tauri`. The binary supplies only what it alone
//! knows — its own paths, version, executable, and build stamp — through
//! [`Environment`].

pub mod args;
pub mod ops;
pub mod report;

use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use observer_pl::ca;

use crate::observe::{DialCounts, OperationObserver};
use args::{ArgError, Command};
use report::{Artifact, Dials, Evidence, Failure, Outcome, Phase};

pub use args::{is_selected, Operation, HELP, MODE_FLAG};
pub use report::SCHEMA_VERSION;

/// Facts only the binary knows. The library never guesses at the executable, the
/// profile layout, or the repository.
#[derive(Debug, Clone)]
pub struct Environment {
    /// Where the paired credential persists — the app's normal `pairing.json`.
    pub state_path: PathBuf,
    /// The sealed-segments root the production uploader drains.
    pub segments_root: PathBuf,
    /// CN placed on the pairing CSR, and the registered device label.
    pub device_label: String,
    pub platform: String,
    pub stream_type: String,
    pub app_version: String,
    /// Segment rotation period, matching the capture engine's.
    pub period_secs: u64,
    /// The running executable, hashed for artifact identity when readable.
    pub executable: Option<PathBuf>,
    /// The build-time `SOLSTONE_SOURCE_COMMIT`, passed through unvalidated.
    pub source_commit: Option<String>,
}

impl Environment {
    /// The `.tmp` sibling `PairedState::save` writes before renaming into place.
    pub fn state_tmp_path(&self) -> PathBuf {
        self.state_path.with_extension("json.tmp")
    }

    fn artifact(&self) -> Artifact {
        Artifact {
            source_commit: validate_source_commit(self.source_commit.as_deref()),
            app_version: self.app_version.clone(),
            executable_sha256: self
                .executable
                .as_ref()
                .and_then(|path| std::fs::read(path).ok())
                .map(|bytes| ca::sha256_hex(&bytes)),
        }
    }
}

/// A build stamp is a full lowercase 40-hex commit or it is nothing.
///
/// Absent or malformed input becomes `None`, never a placeholder and never a
/// fabricated value — the honest-state rule applies to build identity too.
pub fn validate_source_commit(raw: Option<&str>) -> Option<String> {
    let value = raw?.trim();
    let valid = value.len() == 40
        && value
            .chars()
            .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c));
    valid.then(|| value.to_string())
}

/// The parsed mode invocation, or the rejection that still owes an envelope.
pub struct Selection {
    parsed: Result<Command, ArgError>,
}

/// Select the mode from the process arguments, if it was asked for.
pub fn selected<S: AsRef<str>>(args: &[S]) -> Option<Selection> {
    is_selected(args).then(|| Selection {
        parsed: args::parse(args),
    })
}

/// Run the selected operation and produce its single envelope plus exit status.
///
/// This never returns an error: every path — including a bad flag, a failed
/// precondition, and a panic inside an operation — resolves to one envelope, so
/// the stdout contract cannot be violated by a code path forgetting to report.
pub fn report_for(selection: Selection, environment: &Environment) -> Outcome {
    let started = Instant::now();
    let artifact = environment.artifact();

    let command = match selection.parsed {
        Ok(command) => command,
        Err(error) => {
            return report::finish(
                error.operation,
                Some(Failure::error(
                    Phase::Validate,
                    error.reason,
                    &error.guidance,
                )),
                elapsed_ms(started),
                artifact,
                Dials::default(),
                Evidence::default(),
            );
        }
    };

    let operation = command.operation.as_str();
    let observer = OperationObserver::new();
    let max_dials = command.max_dials;

    let executed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        ops::execute(&command, environment, observer.clone())
    }));

    let (failure, evidence) = match executed {
        Ok(outcome) => outcome,
        Err(_) => (
            // A panic is a defect, not a product verdict — but it still owes the
            // caller exactly one object rather than a silent empty stdout.
            Some(Failure::error(
                Phase::Complete,
                "internal_panic",
                "the operation panicked; this is a defect in the integration mode, not a journal fault",
            )),
            Evidence::default(),
        ),
    };

    let counts = observer.counts();
    let failure = failure.or_else(|| dial_maximum_failure(counts, max_dials));

    report::finish(
        operation,
        failure,
        elapsed_ms(started),
        artifact,
        Dials::from_counts(counts, max_dials),
        evidence,
    )
}

/// Run the mode end to end: exactly one JSON object on stdout, exit status back.
///
/// Progress and diagnostics have already gone to stderr by the time this writes.
/// A rejected invocation additionally gets the operator help on stderr — never on
/// stdout, which carries the envelope and nothing else.
pub fn run_to_stdout(selection: Selection, environment: &Environment) -> u8 {
    if selection.parsed.is_err() {
        let _ = writeln!(std::io::stderr(), "{HELP}");
    }
    let outcome = report_for(selection, environment);
    let mut stdout = std::io::stdout().lock();
    // One write, one newline, one object — even if the write itself fails, no
    // second object is ever emitted.
    let _ = writeln!(stdout, "{}", outcome.json());
    let _ = stdout.flush();
    outcome.exit_code
}

/// Exceeding an asserted dial ceiling is a failure, never a pass with truncated
/// evidence.
fn dial_maximum_failure(counts: DialCounts, max_dials: Option<u64>) -> Option<Failure> {
    let max = max_dials?;
    (counts.dial_attempts > max).then(|| {
        Failure::assertion(
            Phase::Assert,
            "dial_maximum_exceeded",
            "the operation dialed more times than --max-dials allows; the evidence is complete and the ceiling was exceeded",
        )
    })
}

/// The path assertion `roundtrip`, `fetch`, and `upload` all apply: the observed
/// path must be the production relay, with zero direct-path successes.
///
/// Public because it is the single place that decides whether dial evidence
/// counts as relay-carried, and that decision is worth asserting directly.
pub fn relay_path_failure(counts: DialCounts) -> Option<Failure> {
    if counts.direct_successes > 0 {
        return Some(Failure::assertion(
            Phase::Assert,
            "direct_path_success",
            "a direct-path request succeeded; this operation must exercise the relay, so make the journal unreachable on the LAN",
        ));
    }
    if counts.relay_successes == 0 {
        return Some(Failure::assertion(
            Phase::Assert,
            "relay_path_not_observed",
            "no relay-path request succeeded, so there is no evidence the relay carried this operation",
        ));
    }
    None
}

pub(crate) fn elapsed_ms(started: Instant) -> u64 {
    started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64
}

/// Build the current-thread runtime the operations run on.
///
/// `pl-transport-win` carries tokio's `rt`, `net`, `time`, `sync`, `macros`, and
/// `io-util` features but **not** `rt-multi-thread`, so a current-thread runtime
/// is the one this crate can construct. It is sufficient: the journal bridge's
/// spawned accept loop and the carrier's reader/writer tasks are polled while
/// `block_on` drives the operation.
pub(crate) fn runtime() -> std::io::Result<tokio::runtime::Runtime> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
}

/// A caller-scoped, fixed local offset.
///
/// `upload` names a day and segment in one explicit frame rather than inheriting
/// the device's DST-sensitive offset, which would make the same caller input mean
/// different instants on different machines.
#[derive(Debug, Clone, Copy)]
pub(crate) struct FixedOffset(pub i64);

impl observer_model::LocalOffset for FixedOffset {
    fn local_offset_secs(&self, _epoch_secs: u64) -> Result<i64, observer_model::LocalOffsetError> {
        Ok(self.0)
    }
}

pub(crate) fn shared_observer(observer: &Arc<OperationObserver>) -> crate::observe::ObserverHandle {
    Some(observer.clone())
}

/// Progress to stderr. Never stdout, which carries exactly one object.
pub(crate) fn progress(message: &str) {
    let _ = writeln!(std::io::stderr(), "integration: {message}");
}

pub(crate) fn deadline_failure(phase: Phase) -> Failure {
    Failure::Deadline { phase }
}

/// Run `future` under the caller's fixed deadline.
pub(crate) async fn with_deadline<F, T>(
    deadline: Duration,
    phase: Phase,
    future: F,
) -> Result<T, Failure>
where
    F: std::future::Future<Output = T>,
{
    tokio::time::timeout(deadline, future)
        .await
        .map_err(|_| deadline_failure(phase))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_commit_is_accepted_only_as_full_lowercase_hex() {
        let good = "0123456789abcdef0123456789abcdef01234567";
        assert_eq!(validate_source_commit(Some(good)), Some(good.to_string()));
        // Surrounding whitespace from a shell export is tolerated.
        assert_eq!(
            validate_source_commit(Some(&format!("  {good}\n"))),
            Some(good.to_string())
        );

        for bad in [
            "",
            "abc",
            &good.to_uppercase(),
            &good[..39],
            &format!("{good}0"),
            "g123456789abcdef0123456789abcdef01234567",
            "not-a-commit",
        ] {
            assert_eq!(validate_source_commit(Some(bad)), None, "{bad:?}");
        }
        assert_eq!(validate_source_commit(None), None);
    }

    #[test]
    fn dial_maximum_is_only_asserted_when_the_operator_set_one() {
        let counts = DialCounts {
            dial_attempts: 9,
            direct_successes: 0,
            relay_successes: 1,
            request_bytes_sent: 0,
            close_completed: true,
        };
        assert!(dial_maximum_failure(counts, None).is_none());
        assert!(dial_maximum_failure(counts, Some(9)).is_none());
        let failure = dial_maximum_failure(counts, Some(8)).unwrap();
        assert_eq!(failure.exit_code(), report::EXIT_ASSERTION_FAILED);
    }

    #[test]
    fn relay_assertion_rejects_a_direct_success_and_a_silent_no_op() {
        let direct = DialCounts {
            dial_attempts: 1,
            direct_successes: 1,
            relay_successes: 0,
            request_bytes_sent: 0,
            close_completed: true,
        };
        let failure = relay_path_failure(direct).unwrap();
        assert_eq!(failure.exit_code(), report::EXIT_ASSERTION_FAILED);

        let nothing = DialCounts {
            dial_attempts: 3,
            direct_successes: 0,
            relay_successes: 0,
            request_bytes_sent: 0,
            close_completed: false,
        };
        assert!(
            relay_path_failure(nothing).is_some(),
            "no relay success is not evidence of a relay path"
        );

        let relay = DialCounts {
            dial_attempts: 3,
            direct_successes: 0,
            relay_successes: 1,
            request_bytes_sent: 0,
            close_completed: true,
        };
        assert!(relay_path_failure(relay).is_none());
    }

    #[test]
    fn a_current_thread_runtime_is_constructible_with_the_crate_features() {
        let runtime = runtime().expect("current-thread runtime");
        // The bridge and carrier rely on spawned tasks making progress under
        // block_on; prove that holds on the runtime the mode actually builds.
        let spawned = runtime.block_on(async {
            let join = tokio::spawn(async {
                tokio::time::sleep(Duration::from_millis(1)).await;
                7u8
            });
            join.await.unwrap()
        });
        assert_eq!(spawned, 7);
    }

    #[test]
    fn fixed_offset_is_stable_across_instants() {
        use observer_model::LocalOffset as _;
        let offset = FixedOffset(0);
        assert_eq!(offset.local_offset_secs(0).unwrap(), 0);
        assert_eq!(offset.local_offset_secs(1_781_706_600).unwrap(), 0);
    }

    #[test]
    fn state_tmp_sibling_matches_what_save_writes() {
        let environment = Environment {
            state_path: PathBuf::from("/profile/Solstone/pairing.json"),
            segments_root: PathBuf::from("/profile/Solstone/segments"),
            device_label: "box".into(),
            platform: "windows".into(),
            stream_type: "desktop".into(),
            app_version: "1.0.0".into(),
            period_secs: 300,
            executable: None,
            source_commit: None,
        };
        assert_eq!(
            environment.state_tmp_path(),
            PathBuf::from("/profile/Solstone/pairing.json.tmp")
        );
    }
}
