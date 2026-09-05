//! A local file accepted for upload.

use super::{DomainError, SafeFileName};
use std::path::PathBuf;

/// What the caller observed a path to be after resolving symbolic links.
///
/// Clift never opens, executes, parses or unpacks an attachment, so the only
/// thing it needs to know is whether the path is an ordinary file. Reading a
/// FIFO or a device would block or have side effects on the user's system.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileKind {
    Regular,
    Directory,
    Symlink,
    BlockDevice,
    CharDevice,
    Socket,
    Fifo,
    Other,
}

impl FileKind {
    #[must_use]
    pub const fn describe(self) -> &'static str {
        match self {
            FileKind::Regular => "a regular file",
            FileKind::Directory => "a directory",
            FileKind::Symlink => "an unresolved symbolic link",
            FileKind::BlockDevice => "a block device",
            FileKind::CharDevice => "a character device",
            FileKind::Socket => "a socket",
            FileKind::Fifo => "a named pipe",
            FileKind::Other => "not an ordinary file",
        }
    }
}

/// A local file that passed validation and may be uploaded.
///
/// The filesystem lookup happens in the adapter that produced `kind`, `size`
/// and `path`; this type performs no IO. What it guarantees is that no code
/// downstream can hold an attachment that is a directory, a device or a
/// relative path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalAttachment {
    path: PathBuf,
    name: SafeFileName,
    size: u64,
}

impl LocalAttachment {
    /// # Errors
    /// Rejects relative paths and anything that is not a regular file. A
    /// directory is rejected with an actionable message because copying folders
    /// is a common attempt.
    pub fn new(
        path: PathBuf,
        name: SafeFileName,
        size: u64,
        kind: FileKind,
    ) -> Result<Self, DomainError> {
        let subject = "attachment";

        if !path.is_absolute() {
            return Err(DomainError::new(
                subject,
                "path must be absolute; resolve it before constructing an attachment",
            ));
        }
        match kind {
            FileKind::Regular => {}
            FileKind::Directory => {
                return Err(DomainError::new(
                    subject,
                    "directories cannot be sent; create an archive first",
                ));
            }
            other => {
                return Err(DomainError::new(
                    subject,
                    format!("must be a regular file, but it is {}", other.describe()),
                ));
            }
        }
        Ok(Self { path, name, size })
    }

    #[must_use]
    pub fn path(&self) -> &PathBuf {
        &self.path
    }

    #[must_use]
    pub fn name(&self) -> &SafeFileName {
        &self.name
    }

    #[must_use]
    pub const fn size(&self) -> u64 {
        self.size
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn name() -> SafeFileName {
        SafeFileName::new("shot.png").unwrap_or_else(|e| panic!("{e}"))
    }

    #[test]
    fn accepts_an_absolute_regular_file() {
        let attachment = LocalAttachment::new(
            PathBuf::from("/Users/dev/shot.png"),
            name(),
            1234,
            FileKind::Regular,
        )
        .unwrap();
        assert_eq!(attachment.size(), 1234);
        assert_eq!(attachment.name().as_str(), "shot.png");
    }

    #[test]
    fn accepts_a_zero_byte_file() {
        assert!(
            LocalAttachment::new(
                PathBuf::from("/tmp/empty.txt"),
                name(),
                0,
                FileKind::Regular
            )
            .is_ok(),
            "a 0 byte file is still a legitimate regular file"
        );
    }

    #[test]
    fn rejects_relative_paths() {
        assert!(
            LocalAttachment::new(PathBuf::from("shot.png"), name(), 1, FileKind::Regular).is_err()
        );
        assert!(
            LocalAttachment::new(PathBuf::from("./shot.png"), name(), 1, FileKind::Regular)
                .is_err()
        );
    }

    #[test]
    fn rejects_directories_with_an_actionable_reason() {
        let err = LocalAttachment::new(PathBuf::from("/Users/dev"), name(), 0, FileKind::Directory)
            .unwrap_err();
        assert!(
            err.reason().contains("archive"),
            "message should tell the user what to do instead: {err}"
        );
    }

    #[test]
    fn rejects_every_non_regular_kind() {
        for kind in [
            FileKind::Directory,
            FileKind::Symlink,
            FileKind::BlockDevice,
            FileKind::CharDevice,
            FileKind::Socket,
            FileKind::Fifo,
            FileKind::Other,
        ] {
            assert!(
                LocalAttachment::new(PathBuf::from("/tmp/x"), name(), 1, kind).is_err(),
                "accepted {kind:?}"
            );
        }
    }
}
