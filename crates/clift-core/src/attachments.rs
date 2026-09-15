//! Turning a local path into something Clift is willing to send.
//!
//! Which files may be attached. The rules are all refusals, and each one has a reason:
//!
//! - **not a regular file**: reading a FIFO blocks until someone writes to it,
//!   and reading a device has side effects on the user's machine;
//! - **a directory**: there is no sensible single file to send, and silently
//!   sending the first thing inside would be worse than saying so;
//! - **unreadable**: better a refusal now than a truncated upload later.
//!
//! Symbolic links are followed, because a link to a screenshot is a screenshot.
//! What is uploaded is the target, and the name shown is the link's own, which
//! is the one the user was looking at.
//!
//! No decision here depends on the file's extension. Clift never opens,
//! executes, parses or unpacks an attachment, so its type is not Clift's
//! business.

use crate::domain::{FileKind, LocalAttachment, SafeFileName};
use crate::error::{CliftError, ErrorKind, Remedy, Stage};
use crate::format::instruction::quote;
use crate::places::Platform;
use std::fs::Metadata;
use std::io;
use std::path::{Path, PathBuf};

/// Inspects one local path.
///
/// # Errors
/// Refuses anything that is not a readable regular file, naming what it found
/// instead. A directory gets its own message, because copying a folder is the
/// common mistake and "must be a regular file" does not help with it.
pub fn inspect(path: &Path) -> Result<LocalAttachment, CliftError> {
    // Follows links and makes the path absolute in one step. A relative path
    // would be resolved against whatever directory the process happens to be
    // in, which is not necessarily the one the user meant.
    let resolved = path
        .canonicalize()
        .map_err(|error| unreadable(path, error))?;
    let metadata = std::fs::metadata(&resolved).map_err(|error| unreadable(path, error))?;

    let kind = classify(&metadata);
    if kind == FileKind::Directory {
        return Err(CliftError::new(
            Stage::Attachment,
            ErrorKind::ClipboardRead,
            format!("{} is a folder, and folders cannot be sent", display(path)),
        )
        .with_remedy(archive(path, Platform::current())));
    }

    // The link's own name rather than the target's: it is the one the user was
    // looking at when they copied it.
    let name = file_name(path)
        .or_else(|| file_name(&resolved))
        .ok_or_else(|| {
            CliftError::new(
                Stage::Attachment,
                ErrorKind::ClipboardRead,
                format!("{} has no file name", path.display()),
            )
        })?;

    LocalAttachment::new(resolved, name, metadata.len(), kind)
        .map_err(|error| error.into_clift(Stage::Attachment, ErrorKind::ClipboardRead))
}

/// Inspects every path, refusing the whole set if any one of them fails.
///
/// All or nothing, because a partially accepted selection is a selection the
/// user did not make.
///
/// # Errors
/// Returns the first refusal.
pub fn inspect_all(paths: &[PathBuf]) -> Result<Vec<LocalAttachment>, CliftError> {
    paths.iter().map(|path| inspect(path)).collect()
}

fn file_name(path: &Path) -> Option<SafeFileName> {
    path.file_name()
        .and_then(|name| name.to_str())
        .map(SafeFileName::sanitize)
}

fn display(path: &Path) -> String {
    path.display().to_string()
}

/// A path that cannot be opened. A missing file is the common case, from a
/// mistyped name, and is called what it is.
fn unreadable(path: &Path, error: io::Error) -> CliftError {
    let message = if error.kind() == io::ErrorKind::NotFound {
        format!("{} does not exist", path.display())
    } else {
        format!("cannot read {}", path.display())
    };
    CliftError::new(Stage::Attachment, ErrorKind::ClipboardRead, message)
        .with_source(error)
        .with_remedy(look_at(path, Platform::current()))
}

/// A command that shows what is at `path`, in the shell the user has.
///
/// On Windows that shell is PowerShell, where `ls -l` does not run.
fn look_at(path: &Path, platform: Platform) -> Remedy {
    let path = display(path);
    let command = match platform {
        Platform::Unix => format!("ls -l {}", quote(&path)),
        Platform::Windows => format!("Get-Item -LiteralPath {}", powershell_quote(&path)),
    };
    Remedy::new("Check that the file is there and readable:", command)
}

/// A command that archives the folder at `path` next to it.
fn archive(path: &Path, platform: Platform) -> Remedy {
    let folder = display(path);
    let zip = format!("{folder}.zip");
    let command = match platform {
        Platform::Unix => format!("zip -r {} {}", quote(&zip), quote(&folder)),
        Platform::Windows => format!(
            "Compress-Archive -LiteralPath {} -DestinationPath {}",
            powershell_quote(&folder),
            powershell_quote(&zip)
        ),
    };
    Remedy::new("Make an archive of it and send that:", command)
}

/// Single quotes, the one PowerShell quoting that expands nothing, with a quote
/// inside written twice.
fn powershell_quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', "''"))
}

#[cfg(unix)]
fn classify(metadata: &Metadata) -> FileKind {
    use std::os::unix::fs::FileTypeExt;

    let kind = metadata.file_type();
    if kind.is_file() {
        FileKind::Regular
    } else if kind.is_dir() {
        FileKind::Directory
    } else if kind.is_fifo() {
        FileKind::Fifo
    } else if kind.is_socket() {
        FileKind::Socket
    } else if kind.is_block_device() {
        FileKind::BlockDevice
    } else if kind.is_char_device() {
        FileKind::CharDevice
    } else {
        FileKind::Other
    }
}

#[cfg(not(unix))]
fn classify(metadata: &Metadata) -> FileKind {
    let kind = metadata.file_type();
    if kind.is_file() {
        FileKind::Regular
    } else if kind.is_dir() {
        FileKind::Directory
    } else {
        FileKind::Other
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The offered command has to run where the user is: `ls` and `zip` are
    /// not PowerShell commands, and a Windows path is not a POSIX word.
    #[test]
    fn the_commands_offered_run_in_the_shell_of_the_platform() {
        let windows = Path::new(r"D:\shots\it's here.png");
        assert_eq!(
            look_at(windows, Platform::Windows).command(),
            r"Get-Item -LiteralPath 'D:\shots\it''s here.png'"
        );
        assert_eq!(
            archive(windows, Platform::Windows).command(),
            r"Compress-Archive -LiteralPath 'D:\shots\it''s here.png' -DestinationPath 'D:\shots\it''s here.png.zip'"
        );

        let unix = Path::new("/home/dev/it's here");
        assert_eq!(
            look_at(unix, Platform::Unix).command(),
            r"ls -l '/home/dev/it'\''s here'"
        );
        assert_eq!(
            archive(unix, Platform::Unix).command(),
            r"zip -r '/home/dev/it'\''s here.zip' '/home/dev/it'\''s here'"
        );
    }
}
