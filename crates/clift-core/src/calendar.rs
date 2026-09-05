//! The only calendar arithmetic Clift performs.
//!
//! Two conversions, in opposite directions, are all that is needed: a date
//! directory is named `YYYY-MM-DD`, and a modification time read back from a
//! remote listing has to become an instant. A date library would bring
//! timezones, parsing and formatting that nothing here uses, so these are
//! written out.
//!
//! Everything is UTC. Clift never renders a local time to a user, and the one
//! place a timezone could have crept in -- the remote listing -- is pinned to
//! UTC by the transport adapter.

use std::time::{SystemTime, UNIX_EPOCH};

const SECONDS_PER_DAY: i64 = 86_400;

/// Days since 1970-01-01 for a proleptic Gregorian date.
///
/// Howard Hinnant's civil calendar algorithm, which is exact for any year and
/// needs no lookup tables or leap-year special cases.
#[must_use]
pub fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let year_of_era = year - era * 400;
    let month = i64::from(month);
    let day_of_year = (153 * (month + if month > 2 { -3 } else { 9 }) + 2) / 5 + i64::from(day) - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

/// The inverse of [`days_from_civil`], as `(year, month, day)`.
#[must_use]
pub fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let days = days + 719_468;
    let era = if days >= 0 { days } else { days - 146_096 } / 146_097;
    let day_of_era = days - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = u32::try_from(day_of_year - (153 * month_prime + 2) / 5 + 1).unwrap_or(1);
    let month = u32::try_from(month_prime + if month_prime < 10 { 3 } else { -9 }).unwrap_or(1);
    (if month <= 2 { year + 1 } else { year }, month, day)
}

/// Seconds since the Unix epoch, negative for instants before it.
#[must_use]
pub fn unix_seconds(time: SystemTime) -> i64 {
    match time.duration_since(UNIX_EPOCH) {
        Ok(since) => i64::try_from(since.as_secs()).unwrap_or(i64::MAX),
        Err(before) => -i64::try_from(before.duration().as_secs()).unwrap_or(i64::MAX),
    }
}

/// The UTC date of an instant, as `YYYY-MM-DD`.
///
/// This is the name of a date directory, so it has to sort lexicographically in
/// chronological order: zero padding is not cosmetic.
#[must_use]
pub fn format_date(time: SystemTime) -> String {
    let (year, month, day) = civil_from_days(unix_seconds(time).div_euclid(SECONDS_PER_DAY));
    format!("{year:04}-{month:02}-{day:02}")
}

/// The RFC 3339 instant, in UTC, that `config.toml` records for
/// `last_success_at`.
///
/// UTC with a literal `Z`: a timestamp in the user's local zone would change
/// meaning when they travel, and the field exists to be compared, not admired.
#[must_use]
pub fn format_timestamp(at: SystemTime) -> String {
    let seconds = unix_seconds(at);
    let days = seconds.div_euclid(SECONDS_PER_DAY);
    let time = seconds.rem_euclid(SECONDS_PER_DAY);
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        time / 3600,
        (time % 3600) / 60,
        time % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_timestamp_is_rfc_3339_in_utc() {
        // 2026-08-30T12:34:00Z
        let at = UNIX_EPOCH + std::time::Duration::from_secs(1_788_093_240);
        assert_eq!(format_timestamp(at), "2026-08-30T12:34:00Z");
    }

    #[test]
    fn the_epoch_itself_formats() {
        assert_eq!(format_timestamp(UNIX_EPOCH), "1970-01-01T00:00:00Z");
    }
    use std::time::Duration;

    fn at(seconds: u64) -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(seconds)
    }

    #[test]
    fn the_two_conversions_are_inverses() {
        for days in [-100_000, -1, 0, 1, 19_000, 20_692, 25_000, 100_000] {
            let (year, month, day) = civil_from_days(days);
            assert_eq!(days_from_civil(year, month, day), days, "day {days}");
        }
    }

    #[test]
    fn known_dates_land_where_they_should() {
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        // A leap day, which is where naive arithmetic goes wrong.
        assert_eq!(civil_from_days(days_from_civil(2024, 2, 29)), (2024, 2, 29));
        // 1900 is not a leap year, 2000 is.
        assert_eq!(civil_from_days(days_from_civil(1900, 3, 1)), (1900, 3, 1));
        assert_eq!(civil_from_days(days_from_civil(2000, 2, 29)), (2000, 2, 29));
    }

    #[test]
    fn a_date_directory_name_is_zero_padded_so_it_sorts_chronologically() {
        assert_eq!(format_date(at(0)), "1970-01-01");
        // 2026-08-30 12:34:00 UTC.
        assert_eq!(format_date(at(1_788_093_240)), "2026-08-30");

        let mut names = vec![
            format_date(at(1_788_093_240)),
            format_date(at(1_767_225_600)), // 2026-01-01
            format_date(at(1_775_001_600)), // 2026-03-31
        ];
        let mut sorted = names.clone();
        sorted.sort();
        names.sort_by_key(|name| name.clone());
        assert_eq!(names, sorted);
        assert_eq!(sorted[0], "2026-01-01");
    }

    #[test]
    fn the_last_second_of_a_day_and_the_first_of_the_next_are_different_dates() {
        // 2026-08-30 23:59:59 UTC and one second later.
        assert_eq!(format_date(at(1_788_134_399)), "2026-08-30");
        assert_eq!(format_date(at(1_788_134_400)), "2026-08-31");
    }

    #[test]
    fn instants_before_the_epoch_do_not_wrap_around() {
        let before = UNIX_EPOCH - Duration::from_secs(1);
        assert_eq!(unix_seconds(before), -1);
        assert_eq!(format_date(before), "1969-12-31");
    }
}
