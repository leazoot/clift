//! Every platform Clift cannot read the clipboard on yet.
//!
//! This exists so that the workspace builds on Linux -- the containers, and any
//! CI runner -- while saying plainly that the capability is absent. A silent
//! empty implementation would be worse than a build failure: `clift send
//! --clipboard` would report "nothing to send" on a machine where there was
//! something to send.
//!
//! Linux support is a new module beside `macos` and `windows`, not an edit to
//! either.

use clift_core::error::{CliftError, ErrorKind, Remedy, Stage};
use clift_core::ports::{ClipboardSnapshot, ClipboardSource};

/// A clipboard reader for a platform that has none.
#[derive(Debug, Default)]
pub struct MacClipboard;

impl MacClipboard {
    #[must_use]
    pub fn new() -> Self {
        Self
    }

    /// Always zero here; the macOS reader keeps its temporary files alive
    /// through the same accessor.
    #[must_use]
    pub const fn retained_files(&self) -> usize {
        0
    }
}

impl ClipboardSource for MacClipboard {
    fn read_snapshot(&self) -> Result<ClipboardSnapshot, CliftError> {
        Err(CliftError::new(
            Stage::Clipboard,
            ErrorKind::ClipboardRead,
            format!(
                "reading the clipboard is not implemented on {}",
                std::env::consts::OS
            ),
        )
        .with_remedy(Remedy::new(
            "Name the files instead of using the clipboard:",
            "clift send <file>...",
        )))
    }
}

/// Replacing the clipboard, on a platform where Clift cannot reach it.
///
/// # Errors
/// Always. `--copy` on such a platform must fail loudly rather than report a
/// replacement that did not happen.
pub fn write_text(_text: &str) -> Result<(), CliftError> {
    Err(CliftError::new(
        Stage::Clipboard,
        ErrorKind::ClipboardRead,
        format!(
            "writing to the clipboard is not implemented on {}",
            std::env::consts::OS
        ),
    ))
}

/// Offering an attachment to the clipboard, on a platform where Clift cannot
/// reach it.
///
/// # Errors
/// Always, for the same reason [`write_text`] always fails here.
pub fn write_offer(_offer: &crate::offer::Offer<'_>) -> Result<crate::offer::Written, CliftError> {
    Err(CliftError::new(
        Stage::Clipboard,
        ErrorKind::ClipboardRead,
        format!(
            "writing to the clipboard is not implemented on {}",
            std::env::consts::OS
        ),
    ))
}
