// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Preview document extraction, filtering, and directory loading.

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::SystemTime;

use crate::record::{parse_line, VERSION};
use crate::writer::{member_path, RotatingFileWriter};
use crate::{MAX_FILES, PREFIX};

/// Abstraction over a snapshot of observer log members.
pub trait LogSource {
    fn load(&self, max_tail_bytes: u64) -> Result<LogSnapshot, std::io::Error>;
}

/// A snapshot containing the rotation state and member files.
pub struct LogSnapshot {
    pub stable: bool,
    pub members: Vec<LogMember>,
}

/// A single log member in the rotation chain.
pub struct LogMember {
    pub index: u8,
    pub id: u64,
    pub body: MemberBody,
}

/// Member content state.
pub enum MemberBody {
    Missing,
    Empty,
    Unreadable,
    Partial(Vec<u8>),
    Read(Vec<u8>),
}

/// Limits and query parameters for preview generation.
pub struct PreviewLimits {
    pub anchor: SystemTime,
    pub hours: u64,
    pub max_entries: usize,
    pub max_bytes: usize,
}

/// Observed status of a rotation member.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemberStatus {
    Missing,
    Empty,
    Unreadable,
    Partial,
    Read,
}

/// Status report for an individual member index.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemberReport {
    pub index: u8,
    pub status: MemberStatus,
}

/// Metadata describing the rendered preview.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreviewMeta {
    pub anchor: SystemTime,
    pub hours: u64,
    pub max_entries: usize,
    pub max_bytes: usize,
    pub first: Option<SystemTime>,
    pub last: Option<SystemTime>,
    pub count: usize,
    pub truncated: bool,
    pub members: [MemberReport; MAX_FILES],
    pub malformed: usize,
    pub legacy: usize,
    pub history_reaches_start: bool,
    pub consistent: bool,
}

/// Error returned by [`read`].
#[derive(Debug)]
pub enum PreviewError {
    Io(std::io::Error),
}

impl fmt::Display for PreviewError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(err) => write!(f, "log preview I/O error: {err}"),
        }
    }
}

impl std::error::Error for PreviewError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(err) => Some(err),
        }
    }
}

impl From<std::io::Error> for PreviewError {
    fn from(err: std::io::Error) -> Self {
        Self::Io(err)
    }
}

/// Immutable in-memory rendered preview document and associated metadata.
pub struct Preview {
    bytes: Vec<u8>,
    meta: PreviewMeta,
}

impl Preview {
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    pub fn meta(&self) -> &PreviewMeta {
        &self.meta
    }

    pub fn write_to(&self, mut writer: impl std::io::Write) -> std::io::Result<()> {
        writer.write_all(&self.bytes)?;
        writer.flush()
    }
}

/// Read and filter log members into a sanitized preview document.
pub fn read(source: &impl LogSource, limits: &PreviewLimits) -> Result<Preview, PreviewError> {
    let max_tail = crate::MAX_BYTES.saturating_mul(2);
    let snapshot = source.load(max_tail)?;

    let mut member_slots: Vec<Option<&LogMember>> = vec![None; MAX_FILES];
    let mut duplicate_index = false;
    let mut seen_indexes = [false; MAX_FILES];

    for member in &snapshot.members {
        let idx = member.index as usize;
        if idx >= MAX_FILES {
            continue;
        }
        if seen_indexes[idx] {
            duplicate_index = true;
            continue;
        }
        seen_indexes[idx] = true;
        member_slots[idx] = Some(member);
    }

    let mut duplicate_id = false;
    let mut kept_members: Vec<Option<&LogMember>> = vec![None; MAX_FILES];
    let mut seen_ids: Vec<u64> = Vec::new();

    for idx in 0..MAX_FILES {
        if let Some(member) = member_slots[idx] {
            if member.id != 0 {
                if seen_ids.contains(&member.id) {
                    duplicate_id = true;
                    kept_members[idx] = None;
                    continue;
                }
                seen_ids.push(member.id);
            }
            kept_members[idx] = Some(member);
        }
    }

    let consistent = snapshot.stable && !duplicate_index && !duplicate_id;

    let mut reports = Vec::with_capacity(MAX_FILES);
    for (idx, slot) in member_slots.iter().enumerate().take(MAX_FILES) {
        let status = match slot {
            None => MemberStatus::Missing,
            Some(m) => match &m.body {
                MemberBody::Missing => MemberStatus::Missing,
                MemberBody::Empty => MemberStatus::Empty,
                MemberBody::Unreadable => MemberStatus::Unreadable,
                MemberBody::Partial(_) => MemberStatus::Partial,
                MemberBody::Read(_) => MemberStatus::Read,
            },
        };
        reports.push(MemberReport {
            index: idx as u8,
            status,
        });
    }

    let mut malformed = 0usize;
    let mut legacy = 0usize;
    let mut history_reaches_start = false;

    let window_start = limits.anchor.checked_sub(std::time::Duration::from_secs(
        limits.hours.saturating_mul(3600),
    ));

    struct ValidRecord<'a> {
        ts: SystemTime,
        line: &'a str,
    }

    let mut in_window_records: Vec<ValidRecord> = Vec::new();

    for member in kept_members.iter().flatten() {
        let raw_bytes: &[u8] = match &member.body {
            MemberBody::Read(b) => b.as_slice(),
            MemberBody::Partial(b) => match b.iter().position(|&byte| byte == b'\n') {
                Some(pos) => &b[pos + 1..],
                None => &[],
            },
            _ => continue,
        };

        if raw_bytes.is_empty() {
            continue;
        }

        let (terminated_bytes, unterminated_tail) = if raw_bytes.ends_with(b"\n") {
            (raw_bytes, false)
        } else {
            match raw_bytes.iter().rposition(|&b| b == b'\n') {
                Some(pos) => (&raw_bytes[..=pos], true),
                None => (&[][..], true),
            }
        };

        if unterminated_tail {
            malformed += 1;
        }

        let mut member_lines = Vec::new();
        for line_bytes in terminated_bytes.split(|&b| b == b'\n') {
            if line_bytes.is_empty() {
                continue;
            }
            match std::str::from_utf8(line_bytes) {
                Err(_) => {
                    malformed += 1;
                }
                Ok(line_str) => {
                    if !line_str.starts_with(VERSION) {
                        legacy += 1;
                    } else {
                        match parse_line(line_str) {
                            None => {
                                malformed += 1;
                            }
                            Some(ts) => {
                                if let Some(start) = window_start {
                                    if ts <= start {
                                        history_reaches_start = true;
                                    }
                                }
                                member_lines.push(ValidRecord { ts, line: line_str });
                            }
                        }
                    }
                }
            }
        }

        for rec in member_lines.into_iter().rev() {
            let in_window = rec.ts <= limits.anchor
                && match window_start {
                    Some(start) => rec.ts >= start,
                    None => true,
                };
            if in_window {
                in_window_records.push(rec);
            }
        }
    }

    let in_window_total = in_window_records.len();
    let mut truncated = false;

    if limits.max_entries == 0 || limits.max_bytes == 0 {
        if in_window_total > 0 {
            truncated = true;
        }
        in_window_records.clear();
    } else {
        if in_window_records.len() > limits.max_entries {
            in_window_records.truncate(limits.max_entries);
            truncated = true;
        }

        let mut current_bytes: usize = in_window_records.iter().map(|r| r.line.len() + 1).sum();
        while current_bytes > limits.max_bytes && !in_window_records.is_empty() {
            if let Some(dropped) = in_window_records.pop() {
                current_bytes -= dropped.line.len() + 1;
                truncated = true;
            }
        }
    }

    let (rendered_bytes, first, last, count) = if in_window_records.is_empty() {
        (Vec::new(), None, None, 0)
    } else {
        in_window_records.reverse();
        let count = in_window_records.len();
        let first = Some(in_window_records.first().unwrap().ts);
        let last = Some(in_window_records.last().unwrap().ts);
        let mut out = Vec::new();
        for rec in &in_window_records {
            out.extend_from_slice(rec.line.as_bytes());
            out.push(b'\n');
        }
        (out, first, last, count)
    };

    let meta = PreviewMeta {
        anchor: limits.anchor,
        hours: limits.hours,
        max_entries: limits.max_entries,
        max_bytes: limits.max_bytes,
        first,
        last,
        count,
        truncated,
        members: reports.try_into().unwrap(),
        malformed,
        legacy,
        history_reaches_start,
        consistent,
    };

    Ok(Preview {
        bytes: rendered_bytes,
        meta,
    })
}

/// LogSource implementation reading rotating log files from disk.
pub struct DirectorySource<'a> {
    dir: PathBuf,
    writer: Option<&'a Mutex<RotatingFileWriter>>,
}

impl<'a> DirectorySource<'a> {
    pub fn new(dir: impl AsRef<Path>, writer: Option<&'a Mutex<RotatingFileWriter>>) -> Self {
        Self {
            dir: dir.as_ref().to_path_buf(),
            writer,
        }
    }
}

impl<'a> LogSource for DirectorySource<'a> {
    fn load(&self, max_tail_bytes: u64) -> Result<LogSnapshot, std::io::Error> {
        let _guard = self
            .writer
            .map(|m| m.lock().unwrap_or_else(|e| e.into_inner()));
        let stable = self.writer.is_some();
        let mut members = Vec::with_capacity(MAX_FILES);

        for index in 0..MAX_FILES {
            let path = member_path(&self.dir, PREFIX, index);
            match std::fs::File::open(&path) {
                Err(err) => {
                    let body = if err.kind() == std::io::ErrorKind::NotFound {
                        MemberBody::Missing
                    } else {
                        MemberBody::Unreadable
                    };
                    members.push(LogMember {
                        index: index as u8,
                        id: 0,
                        body,
                    });
                }
                Ok(mut file) => {
                    let (len, id) = match file.metadata() {
                        Ok(m) => {
                            let len = m.len();
                            let mtime = m.modified().ok();
                            let id = compute_file_id(len, mtime);
                            (len, id)
                        }
                        Err(_) => {
                            members.push(LogMember {
                                index: index as u8,
                                id: 0,
                                body: MemberBody::Unreadable,
                            });
                            continue;
                        }
                    };

                    if len == 0 {
                        members.push(LogMember {
                            index: index as u8,
                            id,
                            body: MemberBody::Empty,
                        });
                    } else {
                        let (id, body) = match read_bounded_tail(&mut file, len, max_tail_bytes) {
                            Ok(bytes) if len > max_tail_bytes => (id, MemberBody::Partial(bytes)),
                            Ok(bytes) => (id, MemberBody::Read(bytes)),
                            Err(_) => (0, MemberBody::Unreadable),
                        };
                        members.push(LogMember {
                            index: index as u8,
                            id,
                            body,
                        });
                    }
                }
            }
        }

        Ok(LogSnapshot { stable, members })
    }
}

fn read_bounded_tail(
    reader: &mut (impl std::io::Read + std::io::Seek),
    observed_len: u64,
    max_tail_bytes: u64,
) -> std::io::Result<Vec<u8>> {
    reader.seek(std::io::SeekFrom::Start(
        observed_len.saturating_sub(max_tail_bytes),
    ))?;
    let mut bounded = std::io::Read::take(reader, observed_len.min(max_tail_bytes));
    let mut bytes = Vec::new();
    std::io::Read::read_to_end(&mut bounded, &mut bytes)?;
    Ok(bytes)
}

fn compute_file_id(len: u64, mtime: Option<SystemTime>) -> u64 {
    let nanos = mtime
        .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    let mut id = len.wrapping_mul(0x517cc1b727220a95) ^ nanos.wrapping_mul(0x9e3779b97f4a7c15);
    if id == 0 {
        id = 1;
    }
    id
}

#[cfg(test)]
pub(crate) mod tests {
    use std::cell::RefCell;
    use std::time::Duration;

    use super::*;
    use crate::panic::panic_line;

    #[test]
    fn file_growth_cannot_extend_the_measured_tail_or_small_member() {
        use std::io::{Cursor, Read};
        let data = b"0123456789abcdefghij";
        // Metadata saw twelve bytes, then another writer appended eight.
        let mut growing = Cursor::new(data);
        let bounded = read_bounded_tail(&mut growing, 12, 8).unwrap();
        assert_eq!(bounded, b"456789ab");
        let mut unbounded = Cursor::new(data);
        unbounded.set_position(4);
        let mut control = Vec::new();
        unbounded.read_to_end(&mut control).unwrap();
        assert_eq!(control.len(), 16);
        assert!(control.len() > bounded.len());
        // A file initially smaller than the tail budget also stops at its
        // observed end instead of following newly appended bytes.
        assert_eq!(
            read_bounded_tail(&mut Cursor::new(data), 4, 8).unwrap(),
            b"0123"
        );
    }

    struct InjectedSource {
        snapshot: LogSnapshot,
    }

    impl LogSource for InjectedSource {
        fn load(&self, _max_tail_bytes: u64) -> Result<LogSnapshot, std::io::Error> {
            let members = self
                .snapshot
                .members
                .iter()
                .map(|m| LogMember {
                    index: m.index,
                    id: m.id,
                    body: match &m.body {
                        MemberBody::Missing => MemberBody::Missing,
                        MemberBody::Empty => MemberBody::Empty,
                        MemberBody::Unreadable => MemberBody::Unreadable,
                        MemberBody::Partial(b) => MemberBody::Partial(b.clone()),
                        MemberBody::Read(b) => MemberBody::Read(b.clone()),
                    },
                })
                .collect();
            Ok(LogSnapshot {
                stable: self.snapshot.stable,
                members,
            })
        }
    }

    #[test]
    fn preview_ignores_legacy_malformed_and_foreign_members() {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        let safe_line = panic_line(
            now,
            "main",
            Some("src/main.rs"),
            Some(10),
            Some(20),
            "safe panic message",
        );

        const LEGACY_SENTINEL_1: &str = "SECRET_LEGACY_ONE";
        const LEGACY_SENTINEL_2: &str = "SECRET_LEGACY_TWO";
        const TRAILING_SENTINEL: &str = "SECRET_TRAILING_PIECE";
        const FOREIGN_SENTINEL: &str = "SECRET_FOREIGN_MEMBER_5";

        let mut active_bytes = Vec::new();
        // 1. Legacy lines with sentinel
        active_bytes.extend_from_slice(
            format!("2024-01-01T00:00:00Z legacy info message {LEGACY_SENTINEL_1}\n").as_bytes(),
        );
        // 2. 3-line legacy panic with solstone-log/1 middle line failing grammar
        active_bytes.extend_from_slice(
            format!("thread 'main' panicked at 'oops':\nsolstone-log/1 invalid grammar {LEGACY_SENTINEL_2}\nstack backtrace:\n").as_bytes()
        );
        // 3. Real safe line
        active_bytes.extend_from_slice(safe_line.as_bytes());
        // 4. Invalid UTF-8 bytes on their own terminated line
        active_bytes.extend_from_slice(&[0xFF, 0xFE, 0xFD, b'\n']);
        // 5. Final unterminated fragment
        active_bytes
            .extend_from_slice(format!("{VERSION} incomplete tail {TRAILING_SENTINEL}").as_bytes());

        let source = InjectedSource {
            snapshot: LogSnapshot {
                stable: true,
                members: vec![
                    LogMember {
                        index: 0,
                        id: 1,
                        body: MemberBody::Read(active_bytes),
                    },
                    LogMember {
                        index: 5,
                        id: 2,
                        body: MemberBody::Read(
                            format!("{VERSION} foreign member {FOREIGN_SENTINEL}\n").into_bytes(),
                        ),
                    },
                ],
            },
        };

        let limits = PreviewLimits {
            anchor: now + Duration::from_secs(60),
            hours: 24,
            max_entries: 100,
            max_bytes: 10_000,
        };

        let preview = read(&source, &limits).expect("read succeeds without panic");
        let rendered = String::from_utf8(preview.bytes().to_vec()).expect("valid utf8 rendered");

        assert_eq!(rendered, safe_line);
        assert!(!rendered.contains(LEGACY_SENTINEL_1));
        assert!(!rendered.contains(LEGACY_SENTINEL_2));
        assert!(!rendered.contains(TRAILING_SENTINEL));
        assert!(!rendered.contains(FOREIGN_SENTINEL));

        assert!(preview.meta().legacy > 0);
        assert!(preview.meta().malformed > 0);
        assert_eq!(preview.meta().count, 1);
    }

    #[test]
    fn preview_applies_limits_window_and_member_status() {
        let t_base = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        let t0 = t_base;
        let t1 = t_base + Duration::from_secs(3600 * 2);
        let t2 = t_base + Duration::from_secs(3600 * 3);

        let rec0 = panic_line(t0, "main", Some("src/m.rs"), Some(1), None, "msg0");
        let rec1 = panic_line(t1, "main", Some("src/m.rs"), Some(2), None, "msg1");
        let rec2 = panic_line(t2, "main", Some("src/m.rs"), Some(3), None, "msg2");

        const CUT_SENTINEL: &str = "SECRET_CUT_SENTINEL";
        let mut cut_partial = format!("cut head before newline with {CUT_SENTINEL}\n").into_bytes();
        cut_partial.extend_from_slice(rec2.as_bytes());

        let source = InjectedSource {
            snapshot: LogSnapshot {
                stable: true,
                members: vec![
                    LogMember {
                        index: 0,
                        id: 10,
                        body: MemberBody::Read(rec1.clone().into_bytes()),
                    },
                    LogMember {
                        index: 1,
                        id: 0,
                        body: MemberBody::Empty,
                    },
                    LogMember {
                        index: 2,
                        id: 0,
                        body: MemberBody::Unreadable,
                    },
                    LogMember {
                        index: 3,
                        id: 11,
                        body: MemberBody::Partial(cut_partial),
                    },
                    LogMember {
                        index: 4,
                        id: 0,
                        body: MemberBody::Missing,
                    },
                ],
            },
        };

        // Window with anchor = t2 and hours = 2 (window start is t2 - 2h = t_base + 1h).
        // rec0 is at t_base (outside window), but is not in this snapshot.
        let limits = PreviewLimits {
            anchor: t2,
            hours: 2,
            max_entries: 10,
            max_bytes: 10_000,
        };

        let preview = read(&source, &limits).expect("read ok");
        let rendered = String::from_utf8(preview.bytes().to_vec()).unwrap();
        assert!(!rendered.contains(CUT_SENTINEL));

        // Member status reports verification
        assert_eq!(preview.meta().members[0].status, MemberStatus::Read);
        assert_eq!(preview.meta().members[1].status, MemberStatus::Empty);
        assert_eq!(preview.meta().members[2].status, MemberStatus::Unreadable);
        assert_eq!(preview.meta().members[3].status, MemberStatus::Partial);
        assert_eq!(preview.meta().members[4].status, MemberStatus::Missing);

        // Snapshot with t0 outside window to test history_reaches_start
        let mut body0 = rec0.into_bytes();
        body0.extend_from_slice(rec1.as_bytes());
        let source_with_t0 = InjectedSource {
            snapshot: LogSnapshot {
                stable: true,
                members: vec![LogMember {
                    index: 0,
                    id: 20,
                    body: MemberBody::Read(body0),
                }],
            },
        };
        // Window start is t_base + 1h. t0 <= start, so history_reaches_start must be true.
        let prev_reaches = read(&source_with_t0, &limits).expect("read ok");
        assert!(prev_reaches.meta().history_reaches_start);

        // Fresh log with only line inside window -> history_reaches_start is false
        let source_fresh = InjectedSource {
            snapshot: LogSnapshot {
                stable: true,
                members: vec![LogMember {
                    index: 0,
                    id: 21,
                    body: MemberBody::Read(rec2.clone().into_bytes()),
                }],
            },
        };
        let prev_fresh = read(&source_fresh, &limits).expect("read ok");
        assert!(!prev_fresh.meta().history_reaches_start);

        // Entry cap test: max_entries = 1 keeps only the newest record (rec1)
        let limits_cap1 = PreviewLimits {
            anchor: t2,
            hours: 2,
            max_entries: 1,
            max_bytes: 10_000,
        };
        let prev_cap1 = read(&source, &limits_cap1).expect("read ok");
        assert_eq!(prev_cap1.bytes(), rec1.as_bytes());
        let rendered_cap1 = String::from_utf8(prev_cap1.bytes().to_vec()).unwrap();
        assert!(!rendered_cap1
            .lines()
            .any(|l| l == rec2.trim_end_matches('\n')));
        assert_eq!(prev_cap1.meta().count, 1);
        assert!(prev_cap1.meta().truncated);

        // Byte cap test: limits max_bytes so older record is dropped whole, never sliced
        let full_len = preview.bytes().len();
        let limits_byte_cap = PreviewLimits {
            anchor: t2,
            hours: 2,
            max_entries: 10,
            max_bytes: full_len - 5,
        };
        let prev_byte_cap = read(&source, &limits_byte_cap).expect("read ok");
        assert!(prev_byte_cap.meta().truncated);
        assert_eq!(prev_byte_cap.bytes(), rec1.as_bytes());
        let rendered_cap = String::from_utf8(prev_byte_cap.bytes().to_vec()).unwrap();
        assert!(!rendered_cap.contains(rec2.trim_end_matches('\n')));
        assert!(!rec2.contains(&rendered_cap));
        assert!(rendered_cap.ends_with('\n'));
        assert_eq!(rendered_cap.lines().count(), 1);
    }

    #[test]
    fn preview_dedupes_rotated_generation() {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        let line_a = panic_line(now, "main", Some("src/a.rs"), Some(1), None, "msg_a");
        let line_b = panic_line(
            now + Duration::from_secs(10),
            "main",
            Some("src/b.rs"),
            Some(2),
            None,
            "msg_b",
        );

        // Id 7 on index 0 and index 1, stable: false -> duplicate id dropped, consistent: false
        let source_dup = InjectedSource {
            snapshot: LogSnapshot {
                stable: false,
                members: vec![
                    LogMember {
                        index: 0,
                        id: 7,
                        body: MemberBody::Read(line_a.clone().into_bytes()),
                    },
                    LogMember {
                        index: 1,
                        id: 7,
                        body: MemberBody::Read(line_a.clone().into_bytes()),
                    },
                ],
            },
        };
        let limits = PreviewLimits {
            anchor: now + Duration::from_secs(60),
            hours: 24,
            max_entries: 10,
            max_bytes: 10_000,
        };
        let prev_dup = read(&source_dup, &limits).expect("read ok");
        assert_eq!(prev_dup.meta().count, 1);
        assert!(!prev_dup.meta().consistent);

        // Id 8 on index 0 (newer) and id 7 on index 1 (older), stable: true
        // Chronological order: older member's record line_a first, then active line_b
        let source_clean = InjectedSource {
            snapshot: LogSnapshot {
                stable: true,
                members: vec![
                    LogMember {
                        index: 0,
                        id: 8,
                        body: MemberBody::Read(line_b.clone().into_bytes()),
                    },
                    LogMember {
                        index: 1,
                        id: 7,
                        body: MemberBody::Read(line_a.clone().into_bytes()),
                    },
                ],
            },
        };
        let prev_clean = read(&source_clean, &limits).expect("read ok");
        assert_eq!(prev_clean.meta().count, 2);
        assert!(prev_clean.meta().consistent);
        let rendered = String::from_utf8(prev_clean.bytes().to_vec()).unwrap();
        let expected = format!("{line_a}{line_b}");
        assert_eq!(rendered, expected);
    }

    struct MutableSource {
        cell: RefCell<LogSnapshot>,
    }

    impl LogSource for MutableSource {
        fn load(&self, _max_tail_bytes: u64) -> Result<LogSnapshot, std::io::Error> {
            let borrow = self.cell.borrow();
            let members = borrow
                .members
                .iter()
                .map(|m| LogMember {
                    index: m.index,
                    id: m.id,
                    body: match &m.body {
                        MemberBody::Missing => MemberBody::Missing,
                        MemberBody::Empty => MemberBody::Empty,
                        MemberBody::Unreadable => MemberBody::Unreadable,
                        MemberBody::Partial(b) => MemberBody::Partial(b.clone()),
                        MemberBody::Read(b) => MemberBody::Read(b.clone()),
                    },
                })
                .collect();
            Ok(LogSnapshot {
                stable: borrow.stable,
                members,
            })
        }
    }

    struct FailingWriter;
    impl std::io::Write for FailingWriter {
        fn write(&mut self, _buf: &[u8]) -> std::io::Result<usize> {
            Err(std::io::Error::other("mock write failure"))
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Err(std::io::Error::other("mock flush failure"))
        }
    }

    #[test]
    pub(crate) fn preview_write_to_preserves_snapshot() {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        let line1 = panic_line(now, "main", Some("src/1.rs"), Some(1), None, "msg1");
        let line2 = panic_line(now, "main", Some("src/2.rs"), Some(2), None, "msg2");

        const NON_ASCII_SENTINEL: &str = "SECRET_café_SENTINEL";
        let mut first_body = format!("legacy unversioned {NON_ASCII_SENTINEL}\n").into_bytes();
        first_body.extend_from_slice(line1.as_bytes());

        let source = MutableSource {
            cell: RefCell::new(LogSnapshot {
                stable: true,
                members: vec![LogMember {
                    index: 0,
                    id: 1,
                    body: MemberBody::Read(first_body),
                }],
            }),
        };

        let limits = PreviewLimits {
            anchor: now + Duration::from_secs(60),
            hours: 24,
            max_entries: 10,
            max_bytes: 10_000,
        };

        let preview1 = read(&source, &limits).expect("read 1 ok");
        let bytes1 = preview1.bytes().to_vec();

        // Mutate source to contain line2
        *source.cell.borrow_mut() = LogSnapshot {
            stable: true,
            members: vec![LogMember {
                index: 0,
                id: 2,
                body: MemberBody::Read(line2.clone().into_bytes()),
            }],
        };

        let preview2 = read(&source, &limits).expect("read 2 ok");
        assert_ne!(preview1.bytes(), preview2.bytes());

        // write_to of preview1 writes exactly bytes1
        let mut written = Vec::new();
        preview1.write_to(&mut written).expect("write_to succeeds");
        assert_eq!(written, bytes1);

        // write_to with an error leaves preview1 unchanged
        let mut fail = FailingWriter;
        let _ = preview1.write_to(&mut fail);
        assert_eq!(preview1.bytes(), bytes1.as_slice());

        // Assert non-ASCII sentinel is absent
        assert!(!preview1.bytes().windows(2).any(|w| w == "é".as_bytes()));
        let rendered = String::from_utf8(preview1.bytes().to_vec()).unwrap();
        assert!(!rendered.contains("café"));
        assert!(!rendered.contains("SECRET_café_SENTINEL"));
    }
}
