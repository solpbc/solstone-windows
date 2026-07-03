// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Persistent local logging for the observer.
//!
//! This crate is pure tier: it owns file rotation, structured redaction helpers,
//! frontend error payloads, RFC3339 UTC timestamps, and panic-line persistence.

#![forbid(unsafe_code)]

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use filter::resolve_filter;
use panic::install_panic_hook;
use time::Rfc3339Utc;
use writer::LogMakeWriter;

pub mod filter;
pub mod frontend;
pub mod journal_open;
pub mod panic;
pub mod redact;
pub mod time;
pub mod writer;

pub use frontend::{FrontendErrorKind, FrontendErrorRecord, FrontendLevel, FrontendOrigin};
pub use journal_open::{
    classify_journal_open_failure, strip_cap, usable_failure_reason, JournalOpenFailure,
    UsableFailureReason,
};
pub use redact::{redact_pair_link, redact_secret, redact_titles, RedactedSecret, TitleSummary};
pub use time::format_rfc3339_utc;
pub use writer::RotatingFileWriter;

/// Log file prefix. Active file is `solstone.log`.
pub const PREFIX: &str = "solstone";
/// Maximum bytes in the active file before rolling to `.1.log`.
pub const MAX_BYTES: u64 = 5 * 1024 * 1024;
/// Active file counts toward this total: active plus `.1.log` through `.4.log`.
pub const MAX_FILES: usize = 5;

/// Pure active-log path helper. Opens nothing.
pub fn active_log_path(dir: &Path) -> PathBuf {
    dir.join(format!("{PREFIX}.log"))
}

/// Install persistent file logging and the panic hook.
///
/// This is intentionally best-effort. If the log directory or global subscriber
/// cannot be installed, logging degrades to no-op and the app keeps booting.
pub fn init(dir: &Path, rust_log: Option<&str>) {
    if let Err(error) = try_init(dir, rust_log) {
        #[cfg(debug_assertions)]
        {
            use std::io::Write as _;

            let _ = writeln!(std::io::stderr(), "observer-log: disabled: {error}");
        }
    }
}

fn try_init(dir: &Path, rust_log: Option<&str>) -> Result<(), InitError> {
    std::fs::create_dir_all(dir).map_err(InitError::CreateDir)?;
    let writer = RotatingFileWriter::new(dir, PREFIX, MAX_BYTES, MAX_FILES)
        .map_err(InitError::OpenWriter)?;
    let writer = Arc::new(Mutex::new(writer));
    let make_writer = LogMakeWriter::new(Arc::clone(&writer));

    tracing_subscriber::fmt()
        .compact()
        .with_ansi(false)
        .with_timer(Rfc3339Utc)
        .with_env_filter(resolve_filter(rust_log))
        .with_writer(make_writer)
        .try_init()
        .map_err(|error| InitError::Subscriber(error.to_string()))?;

    install_panic_hook(writer);
    Ok(())
}

#[derive(Debug)]
enum InitError {
    CreateDir(std::io::Error),
    OpenWriter(std::io::Error),
    Subscriber(String),
}

impl fmt::Display for InitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::CreateDir(error) => write!(f, "create log dir failed: {error}"),
            Self::OpenWriter(error) => write!(f, "open log writer failed: {error}"),
            Self::Subscriber(error) => write!(f, "subscriber install failed: {error}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, UNIX_EPOCH};

    use sha2::{Digest, Sha256};
    use tracing_subscriber::filter::LevelFilter;
    use tracing_subscriber::fmt::MakeWriter;

    use super::*;
    use crate::filter::resolve_filter;
    use crate::frontend::{FrontendErrorKind, FrontendErrorRecord, FrontendLevel, FrontendOrigin};
    use crate::panic::panic_line;
    use crate::redact::{redact_pair_link, redact_secret, redact_titles};
    use crate::time::format_rfc3339_utc;
    use crate::writer::RotatingFileWriter;

    static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

    fn temp_dir(name: &str) -> PathBuf {
        let suffix = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "obslog-test-{}-{}-{}",
            std::process::id(),
            name,
            suffix
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create test dir");
        dir
    }

    fn read(path: impl AsRef<Path>) -> String {
        std::fs::read_to_string(path).expect("read test file")
    }

    #[test]
    fn active_log_path_returns_active_file() {
        let dir = temp_dir("active-path");
        assert_eq!(active_log_path(&dir), dir.join("solstone.log"));
        assert!(!active_log_path(&dir).exists());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn writer_appends_existing_file_and_tracks_size() {
        let dir = temp_dir("append-existing");
        let active = active_log_path(&dir);
        std::fs::write(&active, "seed").expect("seed active");

        let mut writer = RotatingFileWriter::new(&dir, PREFIX, 8, MAX_FILES).expect("writer");
        writer.write_all(b"++").expect("append");
        writer.flush().expect("flush");
        assert_eq!(read(&active), "seed++");

        writer.write_all(b"abc").expect("rotate and write");
        writer.flush().expect("flush");
        assert_eq!(read(&active), "abc");
        assert_eq!(read(dir.join("solstone.1.log")), "seed++");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn writer_rotates_chain_with_active_counting_toward_max_files() {
        let dir = temp_dir("rotate-chain");
        std::fs::write(dir.join("solstone.log"), "active").expect("active");
        std::fs::write(dir.join("solstone.1.log"), "one").expect("one");
        std::fs::write(dir.join("solstone.2.log"), "two").expect("two");
        std::fs::write(dir.join("solstone.3.log"), "three").expect("three");
        std::fs::write(dir.join("solstone.4.log"), "four").expect("four");

        let mut writer = RotatingFileWriter::new(&dir, PREFIX, 6, MAX_FILES).expect("writer");
        writer.write_all(b"z").expect("rotate");
        writer.flush().expect("flush");

        assert_eq!(read(dir.join("solstone.log")), "z");
        assert_eq!(read(dir.join("solstone.1.log")), "active");
        assert_eq!(read(dir.join("solstone.2.log")), "one");
        assert_eq!(read(dir.join("solstone.3.log")), "two");
        assert_eq!(read(dir.join("solstone.4.log")), "three");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn writer_rotates_once_for_oversized_single_buffer() {
        let dir = temp_dir("oversized-once");
        std::fs::write(active_log_path(&dir), "seed").expect("seed active");
        let mut writer = RotatingFileWriter::new(&dir, PREFIX, 5, MAX_FILES).expect("writer");

        writer.write_all(b"0123456789").expect("write oversized");
        writer.flush().expect("flush");

        assert_eq!(read(active_log_path(&dir)), "0123456789");
        assert_eq!(read(dir.join("solstone.1.log")), "seed");
        assert!(!dir.join("solstone.2.log").exists());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn writer_flush_delegates_to_file() {
        let dir = temp_dir("flush");
        let mut writer =
            RotatingFileWriter::new(&dir, PREFIX, MAX_BYTES, MAX_FILES).expect("writer");
        writer.write_all(b"visible").expect("write");
        writer.flush().expect("flush");
        assert_eq!(read(active_log_path(&dir)), "visible");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn make_writer_recovers_poisoned_mutex() {
        let dir = temp_dir("poison");
        let writer = RotatingFileWriter::new(&dir, PREFIX, MAX_BYTES, MAX_FILES).expect("writer");
        let writer = Arc::new(Mutex::new(writer));
        let poison = Arc::clone(&writer);
        let _ = std::thread::spawn(move || {
            let _guard = poison.lock().expect("lock for poison");
            panic!("poison writer mutex");
        })
        .join();

        let make_writer = LogMakeWriter::new(writer);
        let mut guard = make_writer.make_writer();
        guard.write_all(b"after-poison").expect("write");
        guard.flush().expect("flush");
        drop(guard);

        assert_eq!(read(active_log_path(&dir)), "after-poison");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn init_noops_when_dir_is_file() {
        let dir = temp_dir("init-noop");
        let path = dir.join("not-a-dir");
        std::fs::write(&path, "file").expect("file");
        init(&path, None);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn resolve_filter_default_is_info() {
        assert_eq!(
            resolve_filter(None).max_level_hint(),
            Some(LevelFilter::INFO)
        );
    }

    #[test]
    fn resolve_filter_debug_is_debug() {
        assert_eq!(
            resolve_filter(Some("debug")).max_level_hint(),
            Some(LevelFilter::DEBUG)
        );
    }

    #[test]
    fn resolve_filter_invalid_falls_back_to_info() {
        assert_eq!(
            resolve_filter(Some("[")).max_level_hint(),
            Some(LevelFilter::INFO)
        );
    }

    #[test]
    fn frontend_record_rejects_unknown_message_field() {
        let json = r#"{"kind":"error","level":"error","origin":"settings","line":1,"column":2,"message":"secret"}"#;
        assert!(serde_json::from_str::<FrontendErrorRecord>(json).is_err());
    }

    #[test]
    fn frontend_record_accepts_closed_minimal_payload() {
        let json = r#"{"kind":"unhandled_rejection","level":"error","origin":"about","line":0,"column":0}"#;
        let record: FrontendErrorRecord = serde_json::from_str(json).expect("record");
        assert_eq!(record.kind, FrontendErrorKind::UnhandledRejection);
        assert_eq!(record.level, FrontendLevel::Error);
        assert_eq!(record.origin, FrontendOrigin::About);
        assert_eq!(record.line, 0);
        assert_eq!(record.column, 0);
    }

    #[test]
    fn redact_secret_display_keeps_only_kind_len_hash_prefix() {
        let raw = "SEKRIT";
        let redacted = redact_secret("token", raw).to_string();
        let digest = Sha256::digest(raw.as_bytes());
        let expected_prefix = format!(
            "{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
            digest[0], digest[1], digest[2], digest[3], digest[4], digest[5]
        );
        assert_eq!(
            redacted,
            format!("kind=token len=6 sha256={expected_prefix}")
        );
        assert!(!redacted.contains(raw));
    }

    #[test]
    fn redact_pair_link_uses_pair_link_kind() {
        let raw = "pair://host?token=SEKRIT";
        let redacted = redact_pair_link(raw).to_string();
        assert!(redacted.starts_with("kind=pair-link len="));
        assert!(!redacted.contains(raw));
        assert!(!redacted.contains("SEKRIT"));
    }

    #[test]
    fn redact_titles_keeps_only_count() {
        let summary = redact_titles(&["Owner Secret Window Title", "Another"]);
        assert_eq!(summary.to_string(), "titles=2");
        assert!(!summary.to_string().contains("Owner Secret Window Title"));
    }

    #[test]
    fn panic_line_formats_single_line_and_uses_shared_timestamp() {
        let now = UNIX_EPOCH + Duration::from_millis(1_704_067_200_123);
        let line = panic_line(now, "main", Some("src/main.rs:1:2"), "boom");
        assert_eq!(
            line,
            "2024-01-01T00:00:00.123Z PANIC thread=main location=src/main.rs:1:2 message=boom\n"
        );
    }

    #[test]
    fn format_rfc3339_utc_epoch() {
        assert_eq!(format_rfc3339_utc(UNIX_EPOCH), "1970-01-01T00:00:00.000Z");
    }

    #[test]
    fn format_rfc3339_utc_millis_truncates_submillis() {
        let now = UNIX_EPOCH + Duration::new(1, 999_999_999);
        assert_eq!(format_rfc3339_utc(now), "1970-01-01T00:00:01.999Z");
    }

    #[test]
    fn format_rfc3339_utc_leap_day() {
        let now = UNIX_EPOCH + Duration::from_secs(1_709_164_800);
        assert_eq!(format_rfc3339_utc(now), "2024-02-29T00:00:00.000Z");
    }

    #[test]
    fn rfc3339_timer_uses_format_rfc3339_utc() {
        let now = UNIX_EPOCH + Duration::from_millis(1_704_067_200_123);
        assert_eq!(Rfc3339Utc.format_for_test(now), format_rfc3339_utc(now));
    }

    #[test]
    fn ac8_redaction_guard_log_bytes_omit_secret_and_title() {
        let dir = temp_dir("ac8");
        let mut writer =
            RotatingFileWriter::new(&dir, PREFIX, MAX_BYTES, MAX_FILES).expect("writer");
        let pair = redact_pair_link("pair://host?token=SEKRIT");
        let titles = redact_titles(&["Owner Secret Window Title"]);
        let record = FrontendErrorRecord {
            kind: FrontendErrorKind::Error,
            level: FrontendLevel::Error,
            origin: FrontendOrigin::Settings,
            line: 12,
            column: 34,
        };
        writeln!(writer, "{pair}").expect("pair write");
        writeln!(writer, "{titles}").expect("titles write");
        writeln!(
            writer,
            "{}",
            serde_json::to_string(&record).expect("serialize frontend")
        )
        .expect("frontend write");
        writer.flush().expect("flush");

        let bytes = read(active_log_path(&dir));
        assert!(!bytes.contains("SEKRIT"));
        assert!(!bytes.contains("Owner Secret Window Title"));
        let _ = std::fs::remove_dir_all(dir);
    }
}
