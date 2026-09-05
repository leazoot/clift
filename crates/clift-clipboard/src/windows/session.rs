//! Opening the clipboard, and moving bytes in and out of global memory.
//!
//! The Win32 clipboard is a lock: one process holds it at a time, and every
//! read or write happens between `OpenClipboard` and `CloseClipboard`. The
//! [`Session`] type makes that pairing a scope, so no early return can leave
//! the clipboard held and every other application on the machine unable to
//! copy or paste until Clift exits.

use clift_core::error::{CliftError, ErrorKind, Remedy, Stage};
use std::ffi::c_void;
use std::time::Duration;

pub type Handle = *mut c_void;

pub const CF_BITMAP: u32 = 2;
pub const CF_UNICODETEXT: u32 = 13;
pub const CF_HDROP: u32 = 15;
const GMEM_MOVEABLE: u32 = 0x0002;

#[link(name = "user32")]
unsafe extern "system" {
    fn OpenClipboard(owner: Handle) -> i32;
    fn CloseClipboard() -> i32;
    fn EmptyClipboard() -> i32;
    fn GetClipboardData(format: u32) -> Handle;
    fn SetClipboardData(format: u32, data: Handle) -> Handle;
    fn RegisterClipboardFormatW(name: *const u16) -> u32;
}

#[link(name = "kernel32")]
unsafe extern "system" {
    pub fn GlobalAlloc(flags: u32, bytes: usize) -> Handle;
    pub fn GlobalLock(memory: Handle) -> *mut c_void;
    pub fn GlobalUnlock(memory: Handle) -> i32;
    pub fn GlobalSize(memory: Handle) -> usize;
    pub fn GlobalFree(memory: Handle) -> Handle;
}

/// A UTF-16 string with the terminator Win32 expects.
pub fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

/// The clipboard format applications such as browsers and image editors use
/// for PNG bytes. Registered rather than predefined, so its number is asked
/// for by name; asking twice returns the same number.
pub fn png_format() -> u32 {
    let name = wide("PNG");
    // SAFETY: a NUL-terminated wide string that outlives the call.
    unsafe { RegisterClipboardFormatW(name.as_ptr()) }
}

/// The clipboard, held open for as long as this value lives.
pub struct Session(());

impl Session {
    /// Opens the clipboard, waiting briefly if another application has it.
    ///
    /// Another process holds the clipboard for the few milliseconds of its
    /// own copy or paste, and a screenshot tool has it for a moment after the
    /// capture. A handful of short retries covers that; a clipboard held for
    /// longer is a stuck application, and that is reported rather than waited
    /// on forever.
    ///
    /// # Errors
    /// Fails when the clipboard is still held by another process after half
    /// a second.
    pub fn open() -> Result<Self, CliftError> {
        for attempt in 0..20 {
            // SAFETY: a null owner is documented as "the current task"; the
            // clipboard is released by the `Drop` below.
            if unsafe { OpenClipboard(std::ptr::null_mut()) } != 0 {
                return Ok(Self(()));
            }
            if attempt < 19 {
                std::thread::sleep(Duration::from_millis(25));
            }
        }
        Err(CliftError::new(
            Stage::Clipboard,
            ErrorKind::ClipboardRead,
            "another application is holding the clipboard open",
        )
        .with_source(std::io::Error::last_os_error())
        .with_remedy(Remedy::new(
            "Wait a moment and try again:",
            "clift paste --copy",
        )))
    }

    /// The handle behind `format`, owned by the clipboard, or `None` when the
    /// format is not on it.
    pub fn handle(&self, format: u32) -> Option<Handle> {
        // SAFETY: the clipboard is open (this value exists). The handle stays
        // valid until the clipboard is closed or emptied, and is never freed
        // by the caller.
        let handle = unsafe { GetClipboardData(format) };
        (!handle.is_null()).then_some(handle)
    }

    /// A copy of the global-memory block behind `format`.
    ///
    /// # Errors
    /// Fails when the block is present but cannot be locked.
    pub fn bytes(&self, format: u32) -> Result<Option<Vec<u8>>, CliftError> {
        let Some(handle) = self.handle(format) else {
            return Ok(None);
        };
        // SAFETY: `handle` is a live global-memory block owned by the
        // clipboard; the lock is released below before the clipboard is.
        let pointer = unsafe { GlobalLock(handle) };
        if pointer.is_null() {
            return Err(refused("the clipboard's memory block could not be read"));
        }
        // SAFETY: `GlobalSize` is the length of the block `pointer` points at,
        // and the block stays mapped until `GlobalUnlock`.
        let copied = unsafe {
            let size = GlobalSize(handle);
            std::slice::from_raw_parts(pointer.cast::<u8>(), size).to_vec()
        };
        // SAFETY: pairs with the `GlobalLock` above.
        unsafe { GlobalUnlock(handle) };
        Ok(Some(copied))
    }

    /// Removes everything from the clipboard and makes this process its
    /// owner, which is what `SetClipboardData` requires.
    ///
    /// # Errors
    /// Fails when Windows refuses.
    pub fn empty(&self) -> Result<(), CliftError> {
        // SAFETY: the clipboard is open.
        if unsafe { EmptyClipboard() } == 0 {
            return Err(refused("the clipboard could not be emptied"));
        }
        Ok(())
    }

    /// Places a copy of `bytes` on the clipboard under `format`.
    ///
    /// # Errors
    /// Fails when memory cannot be allocated or the clipboard refuses the
    /// data; in the second case the allocation is freed here, because the
    /// clipboard only takes ownership on success.
    pub fn set_bytes(&self, format: u32, bytes: &[u8]) -> Result<(), CliftError> {
        let block = global_block(bytes)?;
        self.set_handle(format, block).inspect_err(|_| {
            // SAFETY: the clipboard did not take the block, so it is still
            // this process's to free.
            unsafe { GlobalFree(block) };
        })
    }

    /// Hands `handle` to the clipboard under `format`. On success the
    /// clipboard owns it; on failure it is still the caller's, to free the
    /// way it was made (a memory block and a bitmap are freed differently).
    ///
    /// # Errors
    /// Fails when the clipboard refuses the handle.
    pub fn set_handle(&self, format: u32, handle: Handle) -> Result<(), CliftError> {
        // SAFETY: the clipboard is open and this process emptied it, which
        // made it the owner; `handle` is a handle of the kind `format` calls
        // for, made by this process and not used again after a success.
        if unsafe { SetClipboardData(format, handle) }.is_null() {
            return Err(refused("the clipboard refused the data"));
        }
        Ok(())
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        // SAFETY: pairs with the successful `OpenClipboard` in `open`.
        unsafe { CloseClipboard() };
    }
}

/// A movable global-memory block holding a copy of `bytes`, which is the
/// only kind of memory the clipboard accepts.
///
/// # Errors
/// Fails when the allocation or the lock fails.
pub fn global_block(bytes: &[u8]) -> Result<Handle, CliftError> {
    // SAFETY: a plain allocation request; a null result is handled.
    let block = unsafe { GlobalAlloc(GMEM_MOVEABLE, bytes.len().max(1)) };
    if block.is_null() {
        return Err(refused("could not allocate clipboard memory"));
    }
    // SAFETY: `block` was just allocated with at least `bytes.len()` bytes.
    let pointer = unsafe { GlobalLock(block) };
    if pointer.is_null() {
        // SAFETY: freeing the block this function allocated and nobody else
        // has seen.
        unsafe { GlobalFree(block) };
        return Err(refused("could not lock clipboard memory"));
    }
    // SAFETY: source and destination do not overlap, and the destination has
    // room for `bytes.len()` bytes.
    unsafe {
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), pointer.cast::<u8>(), bytes.len());
        GlobalUnlock(block);
    }
    Ok(block)
}

pub fn refused(what: &str) -> CliftError {
    CliftError::new(Stage::Clipboard, ErrorKind::ClipboardRead, what.to_string())
        .with_source(std::io::Error::last_os_error())
}
