//! Graph timestamps, held at whole-second precision (ADR-0011 R5).

use std::fmt;

/// A point in time from a Graph response, in whole seconds since the Unix epoch.
///
/// Graph reports times such as `2016-03-21T20:01:37Z`, sometimes with fractional seconds.
/// The fraction is dropped when the value is parsed, so every timestamp that leaves this
/// crate is already at the precision it is stored and compared at (ADR-0011 R5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Timestamp(i64);

impl Timestamp {
    /// Builds a timestamp from seconds since the Unix epoch.
    #[must_use]
    pub const fn from_unix_seconds(seconds: i64) -> Self {
        Self(seconds)
    }

    /// Seconds since the Unix epoch.
    #[must_use]
    pub const fn as_unix_seconds(self) -> i64 {
        self.0
    }

    /// Parses an RFC 3339 date-time (`YYYY-MM-DDTHH:MM:SS[.fff](Z|±HH:MM)`), truncating any
    /// fractional seconds.
    ///
    /// # Errors
    ///
    /// [`TimestampError`] if `text` is not such a date-time.
    pub fn parse_rfc3339(text: &str) -> Result<Self, TimestampError> {
        parse(text.as_bytes()).ok_or(TimestampError)
    }
}

/// A timestamp in a Graph response is not an RFC 3339 date-time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("not an RFC 3339 date-time")]
pub struct TimestampError;

impl fmt::Display for Timestamp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let days = self.0.div_euclid(86_400);
        let secs = self.0.rem_euclid(86_400);
        let (y, m, d) = civil_from_days(days);
        write!(
            f,
            "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
            secs / 3600,
            secs % 3600 / 60,
            secs % 60
        )
    }
}

fn digits(bytes: &[u8], at: usize, len: usize) -> Option<i64> {
    let slice = bytes.get(at..at + len)?;
    slice.iter().try_fold(0i64, |acc, b| {
        b.is_ascii_digit().then(|| acc * 10 + i64::from(b - b'0'))
    })
}

fn expect(bytes: &[u8], at: usize, want: &[u8]) -> Option<()> {
    want.contains(bytes.get(at)?).then_some(())
}

fn parse(b: &[u8]) -> Option<Timestamp> {
    let year = digits(b, 0, 4)?;
    expect(b, 4, b"-")?;
    let month = digits(b, 5, 2)?;
    expect(b, 7, b"-")?;
    let day = digits(b, 8, 2)?;
    expect(b, 10, b"Tt")?;
    let hour = digits(b, 11, 2)?;
    expect(b, 13, b":")?;
    let minute = digits(b, 14, 2)?;
    expect(b, 16, b":")?;
    let second = digits(b, 17, 2)?;
    let mut at = 19;
    if b.get(at) == Some(&b'.') {
        at += 1;
        let start = at;
        while b.get(at).is_some_and(u8::is_ascii_digit) {
            at += 1;
        }
        if at == start {
            return None;
        }
    }
    let offset = match b.get(at)? {
        b'Z' | b'z' => {
            at += 1;
            0
        }
        sign @ (b'+' | b'-') => {
            let h = digits(b, at + 1, 2)?;
            expect(b, at + 3, b":")?;
            let m = digits(b, at + 4, 2)?;
            at += 6;
            if h > 23 || m > 59 {
                return None;
            }
            let total = h * 3600 + m * 60;
            if *sign == b'-' { -total } else { total }
        }
        _ => return None,
    };
    if at != b.len() {
        return None;
    }
    let days_in_month = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if (year % 4 == 0 && year % 100 != 0) || year % 400 == 0 => 29,
        2 => 28,
        _ => return None,
    };
    // A leap second (60) is accepted and folds into the next minute.
    if day < 1 || day > days_in_month || hour > 23 || minute > 59 || second > 60 {
        return None;
    }
    let days = days_from_civil(year, month, day);
    Some(Timestamp(
        days * 86_400 + hour * 3600 + minute * 60 + second - offset,
    ))
}

/// Days since 1970-01-01 of a proleptic Gregorian date (H. Hinnant's algorithm).
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// The inverse of [`days_from_civil`].
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ts(text: &str) -> i64 {
        Timestamp::parse_rfc3339(text).unwrap().as_unix_seconds()
    }

    #[test]
    fn parses_graph_examples() {
        assert_eq!(ts("1970-01-01T00:00:00Z"), 0);
        assert_eq!(ts("2016-03-21T20:01:37Z"), 1_458_590_497);
        assert_eq!(ts("2026-01-01T00:00:00Z"), 1_767_225_600);
        assert_eq!(ts("2000-02-29T12:00:00Z"), 951_825_600);
        assert_eq!(ts("0001-01-01T00:00:00Z"), -62_135_596_800);
    }

    /// ADR-0011 R5: fractional seconds are dropped, never rounded up.
    #[test]
    fn adr_0011_r5_fractional_seconds_are_truncated() {
        assert_eq!(ts("2016-03-21T20:01:37.999Z"), 1_458_590_497);
        assert_eq!(ts("2016-03-21T20:01:37.0000001Z"), 1_458_590_497);
        assert_eq!(ts("1969-12-31T23:59:59.9Z"), -1);
    }

    #[test]
    fn offsets_are_applied() {
        assert_eq!(ts("2016-03-21T21:01:37+01:00"), 1_458_590_497);
        assert_eq!(ts("2016-03-21T19:01:37-01:00"), 1_458_590_497);
    }

    #[test]
    fn rejects_malformed() {
        for bad in [
            "",
            "2016-03-21",
            "2016-03-21T20:01:37",
            "2016-03-21 20:01:37Z",
            "2016-13-21T20:01:37Z",
            "2015-02-29T20:01:37Z",
            "2016-03-21T24:01:37Z",
            "2016-03-21T20:01:37.Z",
            "2016-03-21T20:01:37Zjunk",
            "2016-03-21T20:01:37+0100",
            "２016-03-21T20:01:37Z",
        ] {
            assert!(Timestamp::parse_rfc3339(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn display_round_trips() {
        for text in [
            "1970-01-01T00:00:00Z",
            "2016-03-21T20:01:37Z",
            "0001-01-01T00:00:00Z",
            "2400-02-29T23:59:59Z",
        ] {
            assert_eq!(Timestamp::parse_rfc3339(text).unwrap().to_string(), text);
        }
    }
}
