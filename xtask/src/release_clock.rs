// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Injectable canonical UTC time for release evidence.

use std::fmt;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UtcTimestamp {
    canonical: String,
    instant: SystemTime,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ClockError {
    OutOfRange,
    InvalidFixedTime,
}

impl fmt::Display for ClockError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::OutOfRange => write!(
                formatter,
                "system UTC time is outside the supported release-evidence range; correct the host clock and retry"
            ),
            Self::InvalidFixedTime => write!(
                formatter,
                "fixed test clock is not canonical UTC; use YYYY-MM-DDTHH:MM:SSZ"
            ),
        }
    }
}

impl std::error::Error for ClockError {}

pub trait Clock {
    fn now(&self) -> Result<UtcTimestamp, ClockError>;
}

#[derive(Clone, Copy, Debug, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> Result<UtcTimestamp, ClockError> {
        UtcTimestamp::from_system_time(SystemTime::now())
    }
}

#[derive(Debug)]
pub struct FixedClock {
    fixed: UtcTimestamp,
    calls: AtomicUsize,
}

impl FixedClock {
    pub fn new(canonical: &str) -> Result<Self, ClockError> {
        Ok(Self {
            fixed: UtcTimestamp::parse(canonical)?,
            calls: AtomicUsize::new(0),
        })
    }

    pub fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

impl Clock for FixedClock {
    fn now(&self) -> Result<UtcTimestamp, ClockError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(self.fixed.clone())
    }
}

impl UtcTimestamp {
    pub fn from_system_time(instant: SystemTime) -> Result<Self, ClockError> {
        let seconds = instant
            .duration_since(UNIX_EPOCH)
            .map_err(|_| ClockError::OutOfRange)?
            .as_secs();
        let days = i64::try_from(seconds / 86_400).map_err(|_| ClockError::OutOfRange)?;
        let second_of_day = seconds % 86_400;
        let (year, month, day) = civil_from_days(days);
        if !(1970..=9999).contains(&year) {
            return Err(ClockError::OutOfRange);
        }
        let hour = second_of_day / 3_600;
        let minute = second_of_day % 3_600 / 60;
        let second = second_of_day % 60;
        Ok(Self {
            canonical: format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z"),
            instant,
        })
    }

    pub fn parse(value: &str) -> Result<Self, ClockError> {
        let bytes = value.as_bytes();
        if bytes.len() != 20
            || bytes[4] != b'-'
            || bytes[7] != b'-'
            || bytes[10] != b'T'
            || bytes[13] != b':'
            || bytes[16] != b':'
            || bytes[19] != b'Z'
        {
            return Err(ClockError::InvalidFixedTime);
        }
        let year = parse_decimal(&bytes[0..4])?;
        let month = parse_decimal(&bytes[5..7])?;
        let day = parse_decimal(&bytes[8..10])?;
        let hour = parse_decimal(&bytes[11..13])?;
        let minute = parse_decimal(&bytes[14..16])?;
        let second = parse_decimal(&bytes[17..19])?;
        if year < 1970
            || !(1..=12).contains(&month)
            || day == 0
            || day > days_in_month(year, month)
            || hour > 23
            || minute > 59
            || second > 59
        {
            return Err(ClockError::InvalidFixedTime);
        }
        let days = days_from_civil(year, month, day);
        let seconds = u64::try_from(days)
            .ok()
            .and_then(|days| days.checked_mul(86_400))
            .and_then(|base| base.checked_add(u64::from(hour) * 3_600))
            .and_then(|base| base.checked_add(u64::from(minute) * 60))
            .and_then(|base| base.checked_add(u64::from(second)))
            .ok_or(ClockError::InvalidFixedTime)?;
        Ok(Self {
            canonical: value.to_owned(),
            instant: UNIX_EPOCH + Duration::from_secs(seconds),
        })
    }

    pub fn as_str(&self) -> &str {
        &self.canonical
    }

    pub fn system_time(&self) -> SystemTime {
        self.instant
    }
}

fn parse_decimal(bytes: &[u8]) -> Result<u32, ClockError> {
    if !bytes.iter().all(u8::is_ascii_digit) {
        return Err(ClockError::InvalidFixedTime);
    }
    Ok(bytes
        .iter()
        .fold(0, |value, digit| value * 10 + u32::from(digit - b'0')))
}

fn days_in_month(year: u32, month: u32) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if year.is_multiple_of(400) || (year.is_multiple_of(4) && !year.is_multiple_of(100)) => {
            29
        }
        2 => 28,
        _ => 0,
    }
}

fn days_from_civil(year: u32, month: u32, day: u32) -> i64 {
    let mut year = i64::from(year);
    let month = i64::from(month);
    let day = i64::from(day);
    year -= i64::from(month <= 2);
    let era = year.div_euclid(400);
    let year_of_era = year - era * 400;
    let month_prime = month + if month > 2 { -3 } else { 9 };
    let day_of_year = (153 * month_prime + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let days = days + 719_468;
    let era = days.div_euclid(146_097);
    let day_of_era = days - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let mut year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = month_prime + if month_prime < 10 { 3 } else { -9 };
    year += i64::from(month <= 2);
    (year, month, day)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixed_clock_is_canonical_and_counts_only_observations() {
        let clock = FixedClock::new("2026-07-21T12:34:56Z").expect("create fixed clock");
        assert_eq!(clock.calls(), 0);
        let now = clock.now().expect("read fixed clock");
        assert_eq!(now.as_str(), "2026-07-21T12:34:56Z");
        assert_eq!(clock.calls(), 1);
        assert_eq!(
            UtcTimestamp::from_system_time(now.system_time())
                .expect("round trip fixed instant")
                .as_str(),
            now.as_str()
        );
    }

    #[test]
    fn fixed_clock_rejects_noncanonical_and_invalid_calendar_times() {
        for invalid in [
            "2026-7-21T12:34:56Z",
            "2026-02-29T12:34:56Z",
            "2026-07-21T24:00:00Z",
            "2026-07-21T12:34:60Z",
            "2026-07-21T12:34:56+00:00",
        ] {
            assert_eq!(
                FixedClock::new(invalid).expect_err("invalid fixed time must fail"),
                ClockError::InvalidFixedTime
            );
        }
        FixedClock::new("2024-02-29T00:00:00Z").expect("accept leap day");
    }
}
