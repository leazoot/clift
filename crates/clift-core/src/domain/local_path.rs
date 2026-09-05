//! Absolute path on the machine this process is running on.

use super::DomainError;
use crate::places::Platform;
use std::fmt;

/// An absolute path on *this* host, in this host's own notation.
///
/// The sibling of [`super::RemotePath`], and separate from it on purpose. That
/// type is POSIX by definition -- the remote host is POSIX even when Clift runs
/// on Windows -- and its own documentation says local path semantics must never
/// leak into it. `clift fetch` writes onto the host it is running on, so its
/// paths are exactly what that type refuses to carry: drive letters and
/// backslashes.
///
/// Using `RemotePath` for both worked for as long as `fetch` only ran on Linux
/// servers, where the two notations happen to agree. The first Windows machine
/// to redeem a token got "must be absolute" for `C:\Users\...\inbox`, which is
/// absolute. This type exists so that cannot happen again by accident.
///
/// The one invariant kept is the one that is promised to the agent: the path
/// handed out is absolute. Containment inside the inbox is enforced separately,
/// against real `Path` values, before anything is written.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct LocalPath(String);

impl LocalPath {
    /// # Errors
    /// Rejects an empty path and one that is not absolute for `platform`.
    pub fn new(platform: Platform, value: impl Into<String>) -> Result<Self, DomainError> {
        let value = value.into();
        let subject = "local path";

        if value.is_empty() {
            return Err(DomainError::new(subject, "must not be empty"));
        }
        if !is_absolute(platform, &value) {
            return Err(DomainError::new(
                subject,
                "must be absolute; '~' and relative paths must be resolved first",
            ));
        }
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for LocalPath {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// Whether `value` is absolute under `platform`'s rules.
///
/// Decided on the text and on a platform given as an argument, never by
/// `Path::is_absolute`, which answers for the host the code is running on. That
/// is the difference between a rule that can be tested on any machine and one
/// that can only be tested on Windows -- and this rule was wrong on Windows for
/// as long as nothing tested it there.
#[must_use]
pub fn is_absolute(platform: Platform, value: &str) -> bool {
    match platform {
        Platform::Unix => value.starts_with('/'),
        Platform::Windows => {
            let bytes = value.as_bytes();
            let drive = bytes.len() >= 3
                && bytes[0].is_ascii_alphabetic()
                && bytes[1] == b':'
                && (bytes[2] == b'\\' || bytes[2] == b'/');
            // A UNC path (`\\server\share`) is absolute too, and is what a
            // redirected profile directory looks like.
            drive || value.starts_with("\\\\")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Both platforms' rules, checked from whichever machine runs the tests.
    /// The Windows half is the half that was wrong in the field.
    #[test]
    fn each_platform_decides_absoluteness_by_its_own_rules() {
        for good in [
            r"C:\Users\jinfe\AppData\Local\Clift\inbox",
            r"c:/Users/jinfe/AppData/Local/Clift/inbox",
            r"\\fileserver\profiles\jinfe\Clift",
        ] {
            assert!(is_absolute(Platform::Windows, good), "{good}");
            // The same text is not a Unix absolute path, which is exactly the
            // confusion this type exists to prevent.
            assert!(!is_absolute(Platform::Unix, good), "{good}");
        }

        for bad in [r"Users\jinfe", r"C:", r"C:Users", "", r"\single"] {
            assert!(!is_absolute(Platform::Windows, bad), "{bad}");
        }

        assert!(is_absolute(Platform::Unix, "/home/dev/.cache/clift/inbox"));
        assert!(!is_absolute(Platform::Unix, "home/dev"));
        assert!(!is_absolute(Platform::Unix, "~/.cache"));
    }

    #[test]
    fn a_windows_path_is_accepted_for_windows_and_refused_for_unix() {
        let windows = r"C:\Users\jinfe\AppData\Local\Clift\inbox\shot.png";
        assert_eq!(
            LocalPath::new(Platform::Windows, windows)
                .expect("absolute on Windows")
                .as_str(),
            windows
        );

        let error = LocalPath::new(Platform::Unix, windows).expect_err("not absolute on Unix");
        assert!(error.to_string().contains("must be absolute"), "{error}");
    }

    #[test]
    fn an_empty_path_is_refused_before_anything_else() {
        let error = LocalPath::new(Platform::Unix, "").expect_err("empty");
        assert!(error.to_string().contains("must not be empty"), "{error}");
    }
}
