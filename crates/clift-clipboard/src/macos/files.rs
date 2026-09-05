//! File references on the pasteboard.
//!
//! What Finder puts on the clipboard when the user copies files is a list of
//! `file://` URLs. This module turns them into local paths and nothing more:
//! deciding whether a path may be sent -- regular file, not a directory, not a
//! device -- is a domain rule and lives in `clift-core`.

use clift_core::error::{CliftError, ErrorKind, Stage};
use objc2_app_kit::NSPasteboard;
use objc2_foundation::{NSArray, NSURL};
use std::path::PathBuf;

/// The paths the pasteboard is offering, in the order it offers them.
///
/// A URL that is not a file URL is skipped rather than rejected: copying a web
/// page's address should not make `clift send --clipboard` fail, it should make
/// it find no attachments.
///
/// # Errors
/// Fails when a file URL cannot be turned into a path at all, which would mean
/// the pasteboard is describing something Clift cannot name.
pub fn file_urls(pasteboard: &NSPasteboard) -> Result<Vec<PathBuf>, CliftError> {
    // SAFETY: `readObjectsForClasses:options:` returns an autoreleased array of
    // the requested class, or nil when the pasteboard holds nothing of that
    // kind. The class object is the one AppKit itself vends for NSURL, and
    // objc2 checks the element type on the way out.
    let Some(url_class) = objc2::runtime::AnyClass::get(c"NSURL") else {
        // Unreachable in practice: NSURL is registered as soon as Foundation is
        // loaded. Reported rather than asserted, because a panic here would be
        // a crash in a clipboard read.
        return Err(CliftError::new(
            Stage::Clipboard,
            ErrorKind::ClipboardRead,
            "the Objective-C runtime does not know about NSURL",
        ));
    };
    let classes = NSArray::from_slice(&[url_class]);
    let objects = unsafe { pasteboard.readObjectsForClasses_options(&classes, None) };

    let Some(objects) = objects else {
        return Ok(Vec::new());
    };

    let mut paths = Vec::new();
    for object in objects.iter() {
        let Ok(url) = object.downcast::<NSURL>() else {
            continue;
        };
        if !url.isFileURL() {
            continue;
        }
        let Some(path) = url.path() else {
            return Err(CliftError::new(
                Stage::Clipboard,
                ErrorKind::ClipboardRead,
                "the clipboard holds a file reference with no usable path",
            ));
        };
        paths.push(PathBuf::from(path.to_string()));
    }
    Ok(paths)
}
