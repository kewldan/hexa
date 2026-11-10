//! Calendar helpers (proleptic Gregorian, UTC) without a date crate.

use std::time::{SystemTime, UNIX_EPOCH};

/// Broken-down UTC time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Civil {
    pub year: i64,
    pub month: u32,
    pub day: u32,
    pub hour: u32,
    pub minute: u32,
    pub second: u32,
}

/// Days since 1970-01-01 of a civil date (Howard Hinnant's algorithm).
pub(crate) fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let m = i64::from(month);
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + i64::from(day) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Civil time of a Unix timestamp in seconds.
pub(crate) fn civil_from_unix(secs: i64) -> Civil {
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = yoe + era * 400 + i64::from(month <= 2);
    Civil {
        year,
        month,
        day,
        hour: (rem / 3600) as u32,
        minute: (rem % 3600 / 60) as u32,
        second: (rem % 60) as u32,
    }
}

/// Unix seconds of a civil time.
pub(crate) fn unix_from_civil(c: Civil) -> i64 {
    days_from_civil(c.year, c.month, c.day) * 86_400
        + i64::from(c.hour) * 3600
        + i64::from(c.minute) * 60
        + i64::from(c.second)
}

/// Unix seconds of a system time (negative before 1970).
pub(crate) fn unix_seconds(t: SystemTime) -> i64 {
    match t.duration_since(UNIX_EPOCH) {
        Ok(d) => d.as_secs() as i64,
        Err(e) => -(e.duration().as_secs() as i64),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips() {
        for secs in [
            0,
            951_782_400,
            1_369_353_600,
            1_440_938_160,
            4_102_444_799,
            -86_400,
        ] {
            assert_eq!(unix_from_civil(civil_from_unix(secs)), secs);
        }
        let c = civil_from_unix(1_440_938_160);
        assert_eq!(
            (c.year, c.month, c.day, c.hour, c.minute, c.second),
            (2015, 8, 30, 12, 36, 0)
        );
        // Leap day.
        let c = civil_from_unix(951_782_400);
        assert_eq!((c.year, c.month, c.day), (2000, 2, 29));
    }
}
