//! Parsers for the human-friendly units used in `config.toml`.
//!
//! Hand-written rather than pulled from a crate: the grammar is a number plus
//! one of nine suffixes, and a dependency for that would not earn its place in
//! the binary budget.

use crate::domain::DomainError;
use std::time::Duration;

/// Parses sizes such as `50MiB`, `1GiB`, `512KiB` or a bare byte count.
///
/// # Errors
/// Rejects a missing or unknown suffix, a missing number, and any value that
/// overflows a `u64` once scaled.
pub fn parse_size(value: &str) -> Result<u64, DomainError> {
    let subject = "size";
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(DomainError::new(subject, "must not be empty"));
    }

    let split = trimmed
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(trimmed.len());
    let (digits, suffix) = trimmed.split_at(split);
    let suffix = suffix.trim();

    if digits.is_empty() {
        return Err(DomainError::new(
            subject,
            format!("{value:?} does not start with a number"),
        ));
    }
    let Ok(number) = digits.parse::<u64>() else {
        return Err(DomainError::new(
            subject,
            format!("{digits:?} is not a valid number of units"),
        ));
    };

    let multiplier: u64 = match suffix.to_ascii_lowercase().as_str() {
        "" | "b" => 1,
        "kib" | "k" => 1024,
        "mib" | "m" => 1024 * 1024,
        "gib" | "g" => 1024 * 1024 * 1024,
        _ => {
            return Err(DomainError::new(
                subject,
                format!("unknown unit {suffix:?}; use B, KiB, MiB or GiB"),
            ));
        }
    };

    number
        .checked_mul(multiplier)
        .ok_or_else(|| DomainError::new(subject, format!("{value:?} is too large to represent")))
}

/// Parses retention periods such as `24h`, `7d`, `30m` or `45s`.
///
/// # Errors
/// Rejects a missing or unknown suffix, a missing number and overflow.
pub fn parse_duration(value: &str) -> Result<Duration, DomainError> {
    let subject = "duration";
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(DomainError::new(subject, "must not be empty"));
    }

    let split = trimmed
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(trimmed.len());
    let (digits, suffix) = trimmed.split_at(split);
    let suffix = suffix.trim();

    if digits.is_empty() {
        return Err(DomainError::new(
            subject,
            format!("{value:?} does not start with a number"),
        ));
    }
    let Ok(number) = digits.parse::<u64>() else {
        return Err(DomainError::new(
            subject,
            format!("{digits:?} is not a valid number of units"),
        ));
    };

    let seconds: u64 = match suffix.to_ascii_lowercase().as_str() {
        "s" => 1,
        "m" => 60,
        "h" => 60 * 60,
        "d" => 24 * 60 * 60,
        "" => {
            return Err(DomainError::new(
                subject,
                format!("{value:?} has no unit; use s, m, h or d"),
            ));
        }
        _ => {
            return Err(DomainError::new(
                subject,
                format!("unknown unit {suffix:?}; use s, m, h or d"),
            ));
        }
    };

    number
        .checked_mul(seconds)
        .map(Duration::from_secs)
        .ok_or_else(|| DomainError::new(subject, format!("{value:?} is too large to represent")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_binary_size_units() {
        assert_eq!(parse_size("0").unwrap(), 0);
        assert_eq!(parse_size("1024").unwrap(), 1024);
        assert_eq!(parse_size("1B").unwrap(), 1);
        assert_eq!(parse_size("1KiB").unwrap(), 1024);
        assert_eq!(parse_size("50MiB").unwrap(), 50 * 1024 * 1024);
        assert_eq!(parse_size("100MiB").unwrap(), 100 * 1024 * 1024);
        assert_eq!(parse_size("1GiB").unwrap(), 1024 * 1024 * 1024);
    }

    #[test]
    fn size_parsing_is_case_and_space_insensitive() {
        assert_eq!(parse_size(" 50 mib ").unwrap(), 50 * 1024 * 1024);
        assert_eq!(parse_size("50MIB").unwrap(), 50 * 1024 * 1024);
    }

    #[test]
    fn rejects_bad_sizes() {
        for value in ["", "MiB", "50TiB", "50 MB/s", "-1", "1.5MiB", "abc"] {
            assert!(parse_size(value).is_err(), "accepted {value:?}");
        }
    }

    #[test]
    fn rejects_size_overflow() {
        assert!(parse_size("999999999999999999999GiB").is_err());
        assert!(parse_size(&format!("{}GiB", u64::MAX)).is_err());
    }

    #[test]
    fn parses_durations() {
        assert_eq!(parse_duration("45s").unwrap(), Duration::from_secs(45));
        assert_eq!(parse_duration("30m").unwrap(), Duration::from_secs(1800));
        assert_eq!(parse_duration("24h").unwrap(), Duration::from_secs(86_400));
        assert_eq!(parse_duration("7d").unwrap(), Duration::from_secs(604_800));
    }

    #[test]
    fn rejects_bad_durations() {
        for value in ["", "24", "h", "24 hours", "-1h", "1.5h"] {
            assert!(parse_duration(value).is_err(), "accepted {value:?}");
        }
    }
}
