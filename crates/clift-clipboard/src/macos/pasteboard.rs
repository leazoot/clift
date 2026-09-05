//! The one place in Clift that talks to NSPasteboard.
//!
//! Everything Objective-C stops here. What leaves this module is a
//! [`ClipboardSnapshot`] of plain Rust values, which is what lets the rest of
//! the program be tested without a desktop session.
//!
//! The pasteboard is read **once, when the user asks Clift to send something**.
//! There is no watcher, no cache and no history: a password manager's clipboard
//! must not pass through Clift because Clift happened to be running.

use crate::macos::files::file_urls;
use crate::macos::image::{ImageData, read_image};
use clift_core::error::CliftError;
use clift_core::ports::{ClipboardImage, ClipboardSnapshot, ClipboardSource};
use clift_core::runtime::ScratchFile;
use objc2_app_kit::NSPasteboard;
use objc2_foundation::NSString;
use std::sync::Mutex;

/// Reads the macOS general pasteboard.
///
/// Owns the temporary files it writes for clipboard images: they exist for as
/// long as this value does and are removed when it is dropped, so a failed send
/// cannot leave a copy of someone's screenshot behind.
#[derive(Debug, Default)]
pub struct MacClipboard {
    retained: Mutex<Vec<ScratchFile>>,
}

impl MacClipboard {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// How many temporary files this clipboard is currently keeping alive.
    ///
    /// Exposed for the lifetime tests; a caller has no reason to ask.
    #[must_use]
    pub fn retained_files(&self) -> usize {
        self.retained
            .lock()
            .map_or_else(|poisoned| poisoned.into_inner().len(), |files| files.len())
    }

    fn keep(&self, file: ScratchFile) -> std::path::PathBuf {
        let path = file.path().to_path_buf();
        // A poisoned lock means an earlier panic, not a corrupt list: the guard
        // is recovered rather than turned into a failure, because dropping the
        // handle here would delete a file the caller is about to be given.
        let mut retained = self
            .retained
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        retained.push(file);
        path
    }
}

impl ClipboardSource for MacClipboard {
    fn read_snapshot(&self) -> Result<ClipboardSnapshot, CliftError> {
        // The process-wide pasteboard, retained for the duration of this call.
        // objc2 models this binding as safe: there is no precondition beyond
        // AppKit being loaded, which it is whenever this crate is.
        let pasteboard = NSPasteboard::generalPasteboard();

        let text = read_text(&pasteboard);
        let files = file_urls(&pasteboard)?;

        // The specification decides which of these wins; reading both here keeps that
        // decision in `clift-core` where the rule belongs.
        let mut images = Vec::new();
        if files.is_empty()
            && let Some(image) = read_image(&pasteboard)?
        {
            let ImageData {
                mime,
                extension,
                bytes,
            } = image;
            let scratch = ScratchFile::create("clipboard", extension, &bytes)?;
            images.push(ClipboardImage {
                mime,
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

/// The plain-text representation, if the pasteboard offers one.
fn read_text(pasteboard: &NSPasteboard) -> Option<String> {
    // `nil` for a pasteboard that holds no text at all, which objc2 gives back
    // as `None` rather than as an empty string.
    pasteboard
        .stringForType(type_name("public.utf8-plain-text").as_ref())
        .map(|string| string.to_string())
}

/// Wraps a uniform type identifier as the NSString AppKit expects.
pub(crate) fn type_name(identifier: &str) -> objc2::rc::Retained<NSString> {
    NSString::from_str(identifier)
}
