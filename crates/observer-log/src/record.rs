// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Canonical record encoding and grammar scanner.

use std::borrow::Cow;
use std::fmt;
use std::time::SystemTime;

use crate::time::{format_rfc3339_utc, parse_rfc3339_utc};

pub(crate) const VERSION: &str = "solstone-log/1";
pub(crate) const EXCLUDED_LEN_CAP: usize = 256;

const RESERVED_HEADERS: &[&str] = &["ts", "lvl", "tgt", "mod", "file", "loc", "col", "spans"];

/// Relative file path sanitizer for metadata files and panic locations.
pub(crate) fn relative_file(file: &str) -> Option<&str> {
    if file.is_empty()
        || file.starts_with('/')
        || file.contains("..")
        || file.contains(':')
        || file.contains('\\')
    {
        return None;
    }
    for ch in file.chars() {
        if !matches!(ch, 'A'..='Z' | 'a'..='z' | '0'..='9' | '_' | '.' | '/' | '-') {
            return None;
        }
    }
    for part in file.split('/') {
        if part.is_empty() || part == "." || part == ".." {
            return None;
        }
    }
    Some(file)
}

/// Length-cap a string slice at [`EXCLUDED_LEN_CAP`].
pub(crate) fn capped_len_str(value: &str) -> usize {
    value.len().min(EXCLUDED_LEN_CAP)
}

struct CappedCounter {
    count: usize,
    cap: usize,
}

impl fmt::Write for CappedCounter {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        let remaining = self.cap.saturating_sub(self.count);
        self.count += s.len().min(remaining);
        Ok(())
    }
}

/// Length-cap a Debug value at [`EXCLUDED_LEN_CAP`] without intermediate allocation.
pub(crate) fn capped_len_debug(value: &dyn fmt::Debug) -> usize {
    use std::fmt::Write as _;
    let mut counter = CappedCounter {
        count: 0,
        cap: EXCLUDED_LEN_CAP,
    };
    let _ = write!(&mut counter, "{value:?}");
    counter.count
}

/// Length-cap a Display value at [`EXCLUDED_LEN_CAP`] without intermediate allocation.
pub(crate) fn capped_len_display(value: &dyn fmt::Display) -> usize {
    use std::fmt::Write as _;
    let mut counter = CappedCounter {
        count: 0,
        cap: EXCLUDED_LEN_CAP,
    };
    let _ = write!(&mut counter, "{value}");
    counter.count
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum FieldValue {
    Bool(bool),
    I128(i128),
    U128(u128),
    F64(f64),
    Len(usize),
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Field {
    pub name: Cow<'static, str>,
    pub value: FieldValue,
}

pub(crate) struct Header<'a> {
    pub now: SystemTime,
    pub level: &'static str,
    pub target: &'a str,
    pub module: Option<&'a str>,
    pub file: Option<&'a str>,
    pub line: Option<u32>,
    pub column: Option<u32>,
    pub spans: &'a [&'a str],
}

fn is_valid_token(s: &str) -> bool {
    if s.is_empty() || s.contains("..") {
        return false;
    }
    s.chars()
        .all(|c| matches!(c, 'A'..='Z' | 'a'..='z' | '0'..='9' | '_' | '.' | ':' | '-'))
}

fn is_valid_ident(s: &str) -> bool {
    let mut chars = s.chars();
    match chars.next() {
        Some('A'..='Z' | 'a'..='z' | '_') => {}
        _ => return false,
    }
    chars.all(|c| matches!(c, 'A'..='Z' | 'a'..='z' | '0'..='9' | '_'))
}

fn is_valid_float_repr(s: &str) -> bool {
    if s == "nan" || s == "inf" || s == "-inf" {
        return true;
    }
    let bytes = s.as_bytes();
    if bytes.is_empty() {
        return false;
    }
    let mut idx = 0;
    if bytes[idx] == b'-' {
        idx += 1;
        if idx >= bytes.len() {
            return false;
        }
    }
    if bytes[idx] == b'0' {
        idx += 1;
    } else if matches!(bytes[idx], b'1'..=b'9') {
        idx += 1;
        while idx < bytes.len() && bytes[idx].is_ascii_digit() {
            idx += 1;
        }
    } else {
        return false;
    }

    if idx < bytes.len() && bytes[idx] == b'.' {
        idx += 1;
        let frac_start = idx;
        while idx < bytes.len() && bytes[idx].is_ascii_digit() {
            idx += 1;
        }
        if idx == frac_start {
            return false;
        }
    }

    if idx < bytes.len() && (bytes[idx] == b'e' || bytes[idx] == b'E') {
        idx += 1;
        if idx < bytes.len() && (bytes[idx] == b'+' || bytes[idx] == b'-') {
            idx += 1;
        }
        let exp_start = idx;
        while idx < bytes.len() && bytes[idx].is_ascii_digit() {
            idx += 1;
        }
        if idx == exp_start {
            return false;
        }
    }

    idx == bytes.len()
}

/// Encode a single structured log line.
pub(crate) fn encode(header: &Header<'_>, fields: &[Field]) -> String {
    use std::fmt::Write as _;

    let mut buf = String::with_capacity(256);
    let _ = write!(
        buf,
        "{} ts={} lvl={}",
        VERSION,
        format_rfc3339_utc(header.now),
        header.level
    );

    let tgt = if is_valid_token(header.target) {
        header.target
    } else {
        "-"
    };
    let _ = write!(buf, " tgt={tgt}");

    let mod_tok = header.module.filter(|m| is_valid_token(m)).unwrap_or("-");
    let _ = write!(buf, " mod={mod_tok}");

    let file_tok = header.file.and_then(relative_file).unwrap_or("-");
    let _ = write!(buf, " file={file_tok}");

    match header.line {
        Some(loc) => {
            let _ = write!(buf, " loc={loc}");
        }
        None => {
            buf.push_str(" loc=-");
        }
    }

    if let Some(col) = header.column {
        let _ = write!(buf, " col={col}");
    }

    let mut valid_spans: Vec<&str> = Vec::new();
    for span in header.spans {
        if is_valid_ident(span) {
            valid_spans.push(span);
        }
    }
    if !valid_spans.is_empty() {
        buf.push_str(" spans=");
        for (i, span) in valid_spans.iter().enumerate() {
            if i > 0 {
                buf.push('+');
            }
            buf.push_str(span);
        }
    }

    let mut seen_fields: Vec<String> = Vec::new();
    for field in fields {
        let name_str = field.name.as_ref();
        if !is_valid_ident(name_str) {
            continue;
        }
        let emitted_name = if RESERVED_HEADERS.contains(&name_str) {
            format!("f_{name_str}")
        } else {
            name_str.to_string()
        };

        if seen_fields.iter().any(|s| s == &emitted_name) {
            continue;
        }

        match &field.value {
            FieldValue::Bool(b) => {
                let _ = write!(buf, " {emitted_name}=b:{b}");
                seen_fields.push(emitted_name);
            }
            FieldValue::I128(i) => {
                let _ = write!(buf, " {emitted_name}=i:{i}");
                seen_fields.push(emitted_name);
            }
            FieldValue::U128(u) => {
                let _ = write!(buf, " {emitted_name}=u:{u}");
                seen_fields.push(emitted_name);
            }
            FieldValue::F64(f) => {
                if f.is_nan() {
                    let _ = write!(buf, " {emitted_name}=f:nan");
                    seen_fields.push(emitted_name);
                } else if *f == f64::INFINITY {
                    let _ = write!(buf, " {emitted_name}=f:inf");
                    seen_fields.push(emitted_name);
                } else if *f == f64::NEG_INFINITY {
                    let _ = write!(buf, " {emitted_name}=f:-inf");
                    seen_fields.push(emitted_name);
                } else {
                    let repr = format!("{f:?}");
                    if is_valid_float_repr(&repr) {
                        let _ = write!(buf, " {emitted_name}=f:{repr}");
                        seen_fields.push(emitted_name);
                    }
                }
            }
            FieldValue::Len(len) => {
                let capped = (*len).min(EXCLUDED_LEN_CAP);
                let _ = write!(buf, " {emitted_name}=l:{capped}");
                seen_fields.push(emitted_name);
            }
        }
    }

    buf.push('\n');
    buf
}

/// Parse a single log line into its timestamp if valid.
///
/// Returns `None` if the line violates the grammar.
pub(crate) fn parse_line(line: &str) -> Option<SystemTime> {
    if line.contains('\r')
        || line.contains('\n')
        || line.starts_with(' ')
        || line.ends_with(' ')
        || line.is_empty()
    {
        return None;
    }

    let tokens: Vec<&str> = line.split(' ').collect();
    if tokens.iter().any(|t| t.is_empty()) || tokens.len() < 7 {
        return None;
    }

    if tokens[0] != VERSION {
        return None;
    }

    let ts_token = tokens[1].strip_prefix("ts=")?;
    let timestamp = parse_rfc3339_utc(ts_token)?;

    let lvl_token = tokens[2].strip_prefix("lvl=")?;
    if !matches!(
        lvl_token,
        "TRACE" | "DEBUG" | "INFO" | "WARN" | "ERROR" | "PANIC"
    ) {
        return None;
    }

    let tgt_token = tokens[3].strip_prefix("tgt=")?;
    if tgt_token != "-" && !is_valid_token(tgt_token) {
        return None;
    }

    let mod_token = tokens[4].strip_prefix("mod=")?;
    if mod_token != "-" && !is_valid_token(mod_token) {
        return None;
    }

    let file_token = tokens[5].strip_prefix("file=")?;
    if file_token != "-" && relative_file(file_token).is_none() {
        return None;
    }

    let loc_token = tokens[6].strip_prefix("loc=")?;
    if loc_token != "-" && !is_valid_u32_token(loc_token) {
        return None;
    }

    let mut cur = 7;
    if cur < tokens.len() && tokens[cur].starts_with("col=") {
        let col_token = tokens[cur].strip_prefix("col=")?;
        if !is_valid_u32_token(col_token) {
            return None;
        }
        cur += 1;
    }

    if cur < tokens.len() && tokens[cur].starts_with("spans=") {
        let spans_token = tokens[cur].strip_prefix("spans=")?;
        if spans_token.is_empty() {
            return None;
        }
        for span in spans_token.split('+') {
            if !is_valid_ident(span) {
                return None;
            }
        }
        cur += 1;
    }

    let mut seen_fields: Vec<&str> = Vec::new();
    while cur < tokens.len() {
        let token = tokens[cur];
        let (name, type_val) = token.split_once('=')?;
        if !is_valid_ident(name) {
            return None;
        }
        if seen_fields.contains(&name) {
            return None;
        }
        seen_fields.push(name);

        let (typ, val) = type_val.split_once(':')?;
        match typ {
            "b" => {
                if val != "true" && val != "false" {
                    return None;
                }
            }
            "i" => {
                if !is_valid_i128_token(val) {
                    return None;
                }
            }
            "u" => {
                if !is_valid_u128_token(val) {
                    return None;
                }
            }
            "f" => {
                if !is_valid_float_repr(val) || val.parse::<f64>().is_err() {
                    return None;
                }
            }
            "l" => {
                if !is_valid_len_token(val) {
                    return None;
                }
            }
            _ => return None,
        }
        cur += 1;
    }

    Some(timestamp)
}

fn is_valid_u32_token(s: &str) -> bool {
    if s == "0" {
        return true;
    }
    if s.is_empty() || s.starts_with('0') || !s.chars().all(|c| c.is_ascii_digit()) {
        return false;
    }
    s.parse::<u32>().is_ok()
}

fn is_valid_len_token(s: &str) -> bool {
    if s == "0" {
        return true;
    }
    if s.is_empty() || s.starts_with('0') || !s.chars().all(|c| c.is_ascii_digit()) {
        return false;
    }
    s.parse::<usize>().is_ok_and(|len| len <= EXCLUDED_LEN_CAP)
}

fn is_valid_u128_token(s: &str) -> bool {
    if s == "0" {
        return true;
    }
    if s.is_empty() || s.len() > 40 || s.starts_with('0') || !s.chars().all(|c| c.is_ascii_digit())
    {
        return false;
    }
    s.parse::<u128>().is_ok()
}

fn is_valid_i128_token(s: &str) -> bool {
    if s == "0" {
        return true;
    }
    if s.is_empty() || s.len() > 40 {
        return false;
    }
    let digits = if let Some(stripped) = s.strip_prefix('-') {
        if stripped == "0" {
            return false;
        }
        stripped
    } else {
        s
    };
    if digits.is_empty() || digits.starts_with('0') || !digits.chars().all(|c| c.is_ascii_digit()) {
        return false;
    }
    s.parse::<i128>().is_ok()
}

#[cfg(test)]
mod tests {
    use super::parse_line;

    #[test]
    fn excluded_lengths_cannot_exceed_the_producer_cap() {
        let header = "solstone-log/1 ts=2026-10-16T12:00:00.000Z lvl=INFO tgt=example mod=- file=src/example.rs loc=1";
        assert!(parse_line(&format!("{header} message=l:256")).is_some());
        assert!(parse_line(&format!("{header} message=l:257")).is_none());
    }
}
