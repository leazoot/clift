//! File references on the clipboard.
//!
//! What Explorer puts on the clipboard when the user copies files is an
//! `HDROP`, the same structure a drag-and-drop delivers. This module turns it
//! into local paths and nothing more: deciding whether a path may be sent is a
//! domain rule and lives in `clift-core`.

use super::session::{CF_HDROP, Handle, Session};
use clift_core::error::CliftError;
use std::os::windows::ffi::OsStringExt;
use std::path::PathBuf;

#[link(name = "shell32")]
unsafe extern "system" {
    fn DragQueryFileW(drop: Handle, index: u32, name: *mut u16, capacity: u32) -> u32;
}

/// Asking for this index returns the number of files instead of a name.
const COUNT: u32 = 0xFFFF_FFFF;

/// The paths the clipboard is offering, in the order it offers them.
///
/// # Errors
/// Never, at present: an `HDROP` that answers with no names is an empty list,
/// which `clift-core` treats as "no attachments". The signature keeps the
/// door open for the same reasons the macOS reader's does.
pub fn paths(session: &Session) -> Result<Vec<PathBuf>, CliftError> {
    let Some(drop) = session.handle(CF_HDROP) else {
        return Ok(Vec::new());
    };
    // SAFETY: `drop` is a live HDROP owned by the clipboard, which stays open
    // for the whole of this function; a null buffer with the count index is
    // the documented way to ask how many files there are.
    let count = unsafe { DragQueryFileW(drop, COUNT, std::ptr::null_mut(), 0) };

    let mut paths = Vec::with_capacity(count as usize);
    for index in 0..count {
        // SAFETY: as above; a null buffer asks for the length in characters,
        // without the terminator.
        let length = unsafe { DragQueryFileW(drop, index, std::ptr::null_mut(), 0) };
        if length == 0 {
            continue;
        }
        let mut buffer = vec![0u16; length as usize + 1];
        // SAFETY: `buffer` has room for `length` characters plus the
        // terminator, and its capacity is what is passed.
        let written = unsafe {
            DragQueryFileW(
                drop,
                index,
                buffer.as_mut_ptr(),
                u32::try_from(buffer.len()).unwrap_or(u32::MAX),
            )
        };
        buffer.truncate(written as usize);
        paths.push(PathBuf::from(std::ffi::OsString::from_wide(&buffer)));
    }
    Ok(paths)
}
