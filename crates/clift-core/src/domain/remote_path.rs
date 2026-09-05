//! Absolute path on the remote host.

use super::{DomainError, SafeFileName, reject_control_characters};
use std::fmt;

/// An absolute POSIX path on the remote host.
///
/// Deliberately a string rather than a `PathBuf`: the remote host is POSIX even
/// when Clift runs on Windows, and local path semantics (drive letters,
/// backslash separators) must never leak into what is sent over SFTP.
///
/// Containment inside the inbox root is enforced where the root is known, by
/// building every path with [`RemotePath::join`] from a validated root.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RemotePath(String);

impl RemotePath {
    /// # Errors
    /// Rejects relative paths, control characters, and any path containing a
    /// `.` or `..` component, since those defeat prefix-based containment
    /// checks without a round trip to the remote host.
    pub fn new(value: impl Into<String>) -> Result<Self, DomainError> {
        let value = value.into();
        let subject = "remote path";

        if value.is_empty() {
            return Err(DomainError::new(subject, "must not be empty"));
        }
        reject_control_characters(subject, &value)?;
        if !value.starts_with('/') {
            return Err(DomainError::new(
                subject,
                "must be absolute; '~' and relative paths must be resolved first",
            ));
        }
        for component in value.split('/') {
            if component == "." || component == ".." {
                return Err(DomainError::new(
                    subject,
                    "must not contain a '.' or '..' component",
                ));
            }
        }
        if value.contains("//") {
            return Err(DomainError::new(
                subject,
                "must not contain an empty component",
            ));
        }
        if value.len() > 1 && value.ends_with('/') {
            return Err(DomainError::new(subject, "must not end with a separator"));
        }
        Ok(Self(value))
    }

    /// Appends one already validated path component.
    ///
    /// Taking a [`SafeFileName`] rather than a string is what makes escaping the
    /// parent directory unrepresentable: the component cannot contain a
    /// separator or a traversal entry.
    #[must_use]
    pub fn join(&self, component: &SafeFileName) -> Self {
        if self.0 == "/" {
            Self(format!("/{component}"))
        } else {
            Self(format!("{}/{component}", self.0))
        }
    }

    /// Whether this path is the given root or lives underneath it.
    ///
    /// Compares whole components, so `/home/dev/inbox-old` is not treated as
    /// being inside `/home/dev/inbox`.
    #[must_use]
    pub fn is_within(&self, root: &Self) -> bool {
        if self.0 == root.0 {
            return true;
        }
        let prefix = if root.0.ends_with('/') {
            root.0.clone()
        } else {
            format!("{}/", root.0)
        };
        self.0.starts_with(&prefix)
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for RemotePath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn path(value: &str) -> RemotePath {
        RemotePath::new(value).unwrap_or_else(|e| panic!("expected {value:?} to be valid: {e}"))
    }

    #[test]
    fn accepts_absolute_paths() {
        for value in [
            "/",
            "/home/dev",
            "/home/dev/.cache/clift/inbox",
            "/home/dev/文档 目录/a.png",
        ] {
            assert!(RemotePath::new(value).is_ok(), "rejected {value:?}");
        }
    }

    #[test]
    fn rejects_relative_and_tilde_paths() {
        assert!(RemotePath::new("home/dev").is_err());
        assert!(RemotePath::new("~/.cache/clift").is_err());
        assert!(RemotePath::new("").is_err());
    }

    #[test]
    fn rejects_traversal_components() {
        assert!(RemotePath::new("/home/dev/../../etc/passwd").is_err());
        assert!(RemotePath::new("/home/./dev").is_err());
        assert!(RemotePath::new("/..").is_err());
    }

    #[test]
    fn rejects_malformed_separators() {
        assert!(RemotePath::new("/home//dev").is_err());
        assert!(RemotePath::new("/home/dev/").is_err());
    }

    #[test]
    fn rejects_control_characters() {
        assert!(RemotePath::new("/home/dev/a\nb").is_err());
    }

    #[test]
    fn join_cannot_escape_the_parent() {
        let root = path("/home/dev/.cache/clift/inbox");
        let child = root.join(&SafeFileName::new("shot.png").unwrap());
        assert_eq!(child.as_str(), "/home/dev/.cache/clift/inbox/shot.png");
        assert!(child.is_within(&root));
    }

    #[test]
    fn join_handles_the_filesystem_root() {
        let child = path("/").join(&SafeFileName::new("tmp.png").unwrap());
        assert_eq!(child.as_str(), "/tmp.png");
    }

    #[test]
    fn containment_compares_whole_components() {
        let root = path("/home/dev/inbox");
        assert!(path("/home/dev/inbox").is_within(&root));
        assert!(path("/home/dev/inbox/2026-08-30/abc/x.png").is_within(&root));
        assert!(!path("/home/dev/inbox-old/x.png").is_within(&root));
        assert!(!path("/home/dev").is_within(&root));
        assert!(!path("/etc/passwd").is_within(&root));
    }
}
