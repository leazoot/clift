//! The one place in Clift that reads the Windows clipboard.
//!
//! What leaves this module is a [`ClipboardSnapshot`] of plain Rust values,
//! which is what lets the rest of the program be tested without a desktop
//! session.
//!
//! The clipboard is read **once, when the user asks Clift to send something**.
//! There is no watcher, no cache and no history: a password manager's
//! clipboard must not pass through Clift because Clift happened to be running.

use super::files::paths;
use super::image::{ImageData, read_image};
use super::session::{CF_UNICODETEXT, Session};
use clift_core::error::CliftError;
use clift_core::ports::{ClipboardImage, ClipboardSnapshot, ClipboardSource};
use clift_core::runtime::ScratchFile;
use std::sync::Mutex;

/// Reads the Windows clipboard.
///
/// Owns the temporary files it writes for clipboard images: they exist for as
/// long as this value does and are removed when it is dropped, so a failed
/// send cannot leave a copy of someone's screenshot behind.
#[derive(Debug, Default)]
pub struct WindowsClipboard {
    retained: Mutex<Vec<ScratchFile>>,
}

impl WindowsClipboard {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// How many temporary files this clipboard is currently keeping alive.
    #[must_use]
    pub fn retained_files(&self) -> usize {
        self.retained
            .lock()
            .map_or_else(|poisoned| poisoned.into_inner().len(), |files| files.len())
    }

    fn keep(&self, file: ScratchFile) -> std::path::PathBuf {
        let path = file.path().to_path_buf();
        // A poisoned lock means an earlier panic, not a corrupt list: the
        // guard is recovered rather than turned into a failure, because
        // dropping the handle here would delete a file the caller is about to
        // be given.
        let mut retained = self
            .retained
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        retained.push(file);
        path
    }
}

impl ClipboardSource for WindowsClipboard {
    fn read_snapshot(&self) -> Result<ClipboardSnapshot, CliftError> {
        let session = Session::open()?;

        let text = read_text(&session)?;
        let files = paths(&session)?;

        // Which of these wins is decided in `clift-core`; reading both here
        // keeps that rule where it belongs.
        let mut images = Vec::new();
        if files.is_empty()
            && let Some(image) = read_image(&session)?
        {
            let ImageData {
                mime,
                extension,
                bytes,
            } = image;
            let scratch = ScratchFile::create("clipboard", extension, &bytes)?;
            images.push(ClipboardImage {
                mime: mime.to_string(),
                path: self.keep(scratch),
            });
        }

        Ok(ClipboardSnapshot {
            text,
            files,
            images,
        })
    }
}

/// The Unicode text representation, if the clipboard offers one.
///
/// Lossy decoding is deliberate: the text is used to tell "the user copied
/// words" from "the user copied a picture", and one unpaired surrogate must
/// not turn that into a failure.
fn read_text(session: &Session) -> Result<Option<String>, CliftError> {
    let Some(bytes) = session.bytes(CF_UNICODETEXT)? else {
        return Ok(None);
    };
    let characters: Vec<u16> = bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| u16::from_le_bytes(*pair))
        .take_while(|&character| character != 0)
        .collect();
    Ok(Some(String::from_utf16_lossy(&characters)))
}
