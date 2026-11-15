//! The simulator's clock and Graph's timestamp format.
//!
//! Graph timestamps are whole seconds (abraunegg `usage.md`, cited in
//! `graph_api_and_windows_parity.md` section 2), written as `2016-03-21T20:01:37Z` in the
//! documentation's examples. The clock never reads wall-clock time, so a test's responses
//! are the same on every run; tests move it with `GraphSim::advance_clock`.

use crate::GraphSim;

/// Seconds since the Unix epoch at which every simulator's clock starts:
/// 2026-01-01T00:00:00Z.
pub(crate) const START_SECS: i64 = 1_767_225_600;

/// Formats seconds since the Unix epoch as `YYYY-MM-DDTHH:MM:SSZ`.
pub(crate) fn format_timestamp(secs: i64) -> String {
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60
    )
}

/// Converts days since 1970-01-01 to a proleptic Gregorian (year, month, day).
///
/// Howard Hinnant's `civil_from_days` algorithm.
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

impl GraphSim {
    /// Moves the simulator's clock forward. Timestamps are whole seconds and the clock
    /// starts at 2026-01-01T00:00:00Z; it never reads the wall clock.
    pub fn advance_clock(&self, secs: u32) {
        self.shared.lock().clock += i64::from(secs);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_known_instants() {
        assert_eq!(format_timestamp(0), "1970-01-01T00:00:00Z");
        assert_eq!(format_timestamp(START_SECS), "2026-01-01T00:00:00Z");
        // The instant in the Graph get-item example.
        assert_eq!(format_timestamp(1_458_590_497), "2016-03-21T20:01:37Z");
        assert_eq!(format_timestamp(951_782_400), "2000-02-29T00:00:00Z");
    }
}
