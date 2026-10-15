//! UTC dates and times from the system clock, without a date crate: the
//! release job stamps `published` (a TOML date) and `generated` (RFC 3339).

use std::time::{SystemTime, UNIX_EPOCH};

const DAY: i64 = 86_400;

/// Seconds since the Unix epoch, now.
pub fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs() as i64)
        .unwrap_or(0)
}

/// `(year, month, day)` of a day count from 1970-01-01 (proleptic Gregorian).
pub fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let day_of_era = (z - era * 146_097) as u64;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let day = (day_of_year - (153 * month_index + 2) / 5 + 1) as u32;
    let month = if month_index < 10 {
        month_index + 3
    } else {
        month_index - 9
    } as u32;
    let year = year_of_era as i64 + era * 400;
    (if month <= 2 { year + 1 } else { year }, month, day)
}

/// The UTC date of a Unix time, as a TOML date: `YYYY-MM-DD`.
pub fn date_of(unix: i64) -> String {
    let (year, month, day) = civil_from_days(unix.div_euclid(DAY));
    format!("{year:04}-{month:02}-{day:02}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_instants_are_formatted() {
        assert_eq!(date_of(-1), "1969-12-31");
    }

    #[test]
    fn leap_days_are_counted() {
        assert_eq!(date_of(951_782_400), "2000-02-29");
        assert_eq!(date_of(951_782_400 - DAY), "2000-02-28");
        assert_eq!(date_of(951_782_400 + DAY), "2000-03-01");
    }
}
