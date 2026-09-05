//! Alias a user gives a configured host, e.g. `core`.

use super::{DomainError, reject_control_characters};
use std::fmt;

/// A target alias. Used as a TOML table key and printed in error text, so it
/// must be free of control characters and path separators.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TargetName(String);

/// Long enough for any realistic alias; short enough to keep `target list`
/// readable and to bound anything that embeds the name.
const MAX_LEN: usize = 64;

impl TargetName {
    /// # Errors
    /// Rejects empty or whitespace-only names, control characters, path
    /// separators and names longer than 64 bytes.
    pub fn new(value: impl Into<String>) -> Result<Self, DomainError> {
        let value = value.into();
        let subject = "target name";

        if value.is_empty() {
            return Err(DomainError::new(subject, "must not be empty"));
        }
        if value.trim().is_empty() {
            return Err(DomainError::new(subject, "must not be only whitespace"));
        }
        reject_control_characters(subject, &value)?;
        if value.contains('/') || value.contains('\\') {
            return Err(DomainError::new(
                subject,
                "must not contain a path separator",
            ));
        }
        if value.len() > MAX_LEN {
            return Err(DomainError::new(
                subject,
                format!("must be at most {MAX_LEN} bytes, got {}", value.len()),
            ));
        }
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for TargetName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_ordinary_aliases() {
        for name in ["core", "hk", "dev-vps", "build_box", "vm2", "私有主机"] {
            assert!(TargetName::new(name).is_ok(), "rejected {name:?}");
        }
    }

    #[test]
    fn rejects_empty_and_whitespace() {
        assert!(TargetName::new("").is_err());
        assert!(TargetName::new("   ").is_err());
        assert!(TargetName::new("\t").is_err());
    }

    #[test]
    fn rejects_control_characters() {
        assert!(TargetName::new("core\u{0}").is_err());
        assert!(TargetName::new("core\nhk").is_err());
        assert!(TargetName::new("\u{1b}[31mcore").is_err());
    }

    #[test]
    fn rejects_path_separators() {
        assert!(TargetName::new("../core").is_err());
        assert!(TargetName::new("a/b").is_err());
        assert!(TargetName::new("a\\b").is_err());
    }

    #[test]
    fn rejects_overlong_names() {
        assert!(TargetName::new("a".repeat(MAX_LEN)).is_ok());
        assert!(TargetName::new("a".repeat(MAX_LEN + 1)).is_err());
    }
}
