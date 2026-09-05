//! Converting between a GDI bitmap and PNG bytes with GDI+.
//!
//! GDI+ is used for the same reason `NSBitmapImageRep` is on macOS: it is a
//! system imaging capability, present on every Windows installation, and using
//! it keeps the `image` crate and its megabytes out of the binary. Only its
//! flat C interface is called; the few COM calls needed for the in-memory
//! stream go through the interface's vtable directly.
//!
//! Both directions are lossless for what a screenshot contains. A bitmap made
//! from an `HBITMAP` has no alpha channel, which a screen capture does not have
//! either; a PNG with transparency put *back* on the clipboard is composed onto
//! white, the same thing every image editor does with it.

use super::session::{GlobalLock, GlobalSize, GlobalUnlock, Handle, global_block, refused};
use clift_core::error::{CliftError, ErrorKind, Remedy, Stage};
use std::ffi::c_void;

#[repr(C)]
struct StartupInput {
    version: u32,
    debug_callback: *const c_void,
    suppress_background_thread: i32,
    suppress_external_codecs: i32,
}

#[repr(C)]
struct Guid {
    data1: u32,
    data2: u16,
    data3: u16,
    data4: [u8; 8],
}

/// The built-in PNG encoder's class id, from the GDI+ documentation.
static PNG_ENCODER: Guid = Guid {
    data1: 0x557C_F406,
    data2: 0x1A04,
    data3: 0x11D3,
    data4: [0x9A, 0x73, 0x00, 0x00, 0xF8, 0x1E, 0xF3, 0x2E],
};

/// The first six entries of `IStream`'s vtable: the three every COM interface
/// has, then `Read`, `Write` and `Seek`. Only `Release` and `Seek` are called.
#[repr(C)]
struct StreamVtbl {
    query_interface: usize,
    add_ref: usize,
    release: unsafe extern "system" fn(this: *mut c_void) -> u32,
    read: usize,
    write: usize,
    seek: unsafe extern "system" fn(
        this: *mut c_void,
        offset: i64,
        origin: u32,
        position: *mut u64,
    ) -> i32,
}

const STREAM_SEEK_CUR: u32 = 1;

#[link(name = "gdiplus")]
unsafe extern "system" {
    fn GdiplusStartup(token: *mut usize, input: *const StartupInput, output: *mut c_void) -> i32;
    fn GdiplusShutdown(token: usize);
    fn GdipCreateBitmapFromHBITMAP(bitmap: Handle, palette: Handle, out: *mut *mut c_void) -> i32;
    fn GdipCreateBitmapFromStream(stream: *mut c_void, out: *mut *mut c_void) -> i32;
    fn GdipCreateHBITMAPFromBitmap(bitmap: *mut c_void, out: *mut Handle, background: u32) -> i32;
    fn GdipSaveImageToStream(
        image: *mut c_void,
        stream: *mut c_void,
        encoder: *const Guid,
        parameters: *const c_void,
    ) -> i32;
    fn GdipDisposeImage(image: *mut c_void) -> i32;
}

#[link(name = "gdi32")]
unsafe extern "system" {
    fn DeleteObject(object: Handle) -> i32;
}

/// Frees a bitmap made by [`png_to_bitmap`] that the clipboard did not take.
pub fn delete_bitmap(bitmap: Handle) {
    // SAFETY: a GDI object handle this process owns, deleted once, and not
    // selected into any device context.
    unsafe { DeleteObject(bitmap) };
}

#[link(name = "ole32")]
unsafe extern "system" {
    fn CreateStreamOnHGlobal(memory: Handle, delete_on_release: i32, out: *mut *mut c_void) -> i32;
    fn GetHGlobalFromStream(stream: *mut c_void, out: *mut Handle) -> i32;
}

/// GDI+ initialised for the scope of one conversion.
///
/// Declared first in every function below so that it is dropped last: every
/// other handle in that function belongs to GDI+ and must be released before
/// `GdiplusShutdown`.
struct Gdiplus(usize);

impl Gdiplus {
    fn start() -> Result<Self, CliftError> {
        let input = StartupInput {
            version: 1,
            debug_callback: std::ptr::null(),
            suppress_background_thread: 0,
            suppress_external_codecs: 0,
        };
        let mut token = 0usize;
        // SAFETY: `input` is a correctly laid out startup structure and
        // `token` a live out-parameter; the output structure is optional.
        let status =
            unsafe { GdiplusStartup(&raw mut token, &raw const input, std::ptr::null_mut()) };
        if status != 0 {
            return Err(failed("GdiplusStartup", status));
        }
        Ok(Self(token))
    }
}

impl Drop for Gdiplus {
    fn drop(&mut self) {
        // SAFETY: the token came from a successful `GdiplusStartup`.
        unsafe { GdiplusShutdown(self.0) };
    }
}

/// A GDI+ image, disposed when dropped.
struct Image(*mut c_void);

impl Drop for Image {
    fn drop(&mut self) {
        // SAFETY: a live image returned by a GDI+ constructor, disposed once.
        unsafe { GdipDisposeImage(self.0) };
    }
}

/// An `IStream` over global memory, released when dropped.
struct Stream(*mut c_void);

impl Stream {
    /// An empty, growable stream.
    fn new() -> Result<Self, CliftError> {
        let mut stream = std::ptr::null_mut();
        // SAFETY: a null block asks the stream to allocate its own, and
        // `delete_on_release` makes it free that block with itself.
        let result = unsafe { CreateStreamOnHGlobal(std::ptr::null_mut(), 1, &raw mut stream) };
        if result < 0 || stream.is_null() {
            return Err(failed("CreateStreamOnHGlobal", result));
        }
        Ok(Self(stream))
    }

    /// A stream holding a copy of `bytes`.
    fn over(bytes: &[u8]) -> Result<Self, CliftError> {
        let block = global_block(bytes)?;
        let mut stream = std::ptr::null_mut();
        // SAFETY: `block` is a movable global-memory block this function owns;
        // with `delete_on_release` set the stream frees it, so it is not freed
        // here on success. On failure it is leaked rather than double-freed,
        // which for a few hundred kilobytes once is the safer error.
        let result = unsafe { CreateStreamOnHGlobal(block, 1, &raw mut stream) };
        if result < 0 || stream.is_null() {
            return Err(failed("CreateStreamOnHGlobal", result));
        }
        Ok(Self(stream))
    }

    fn vtable(&self) -> &StreamVtbl {
        // SAFETY: a COM interface pointer is a pointer to a pointer to its
        // vtable, and this stream is a live `IStream`.
        unsafe { &**self.0.cast::<*const StreamVtbl>() }
    }

    /// The bytes written so far.
    ///
    /// The backing block is larger than the content (it grows in steps), so
    /// the length is the stream's own position after the encoder finished
    /// writing, not the block's size.
    fn contents(&self) -> Result<Vec<u8>, CliftError> {
        let mut length = 0u64;
        // SAFETY: a seek of zero from the current position on a live stream,
        // which only reports where the position is.
        let result = unsafe { (self.vtable().seek)(self.0, 0, STREAM_SEEK_CUR, &raw mut length) };
        if result < 0 {
            return Err(failed("IStream::Seek", result));
        }
        let length = usize::try_from(length).unwrap_or(usize::MAX);

        let mut block = std::ptr::null_mut();
        // SAFETY: the stream was created over global memory, which is the
        // one case this call is documented to succeed for.
        let result = unsafe { GetHGlobalFromStream(self.0, &raw mut block) };
        if result < 0 || block.is_null() {
            return Err(failed("GetHGlobalFromStream", result));
        }
        // SAFETY: the block stays owned by the stream, which is alive for the
        // whole of this function; it is locked, copied and unlocked here.
        let pointer = unsafe { GlobalLock(block) };
        if pointer.is_null() {
            return Err(refused("could not lock the encoded image"));
        }
        // SAFETY: `length` is the stream's position, which cannot exceed the
        // block's size; that size is checked anyway.
        let copied = unsafe {
            let available = GlobalSize(block);
            std::slice::from_raw_parts(pointer.cast::<u8>(), length.min(available)).to_vec()
        };
        // SAFETY: pairs with the lock above.
        unsafe { GlobalUnlock(block) };
        Ok(copied)
    }
}

impl Drop for Stream {
    fn drop(&mut self) {
        // SAFETY: a live interface pointer with one reference, this one.
        unsafe { (self.vtable().release)(self.0) };
    }
}

/// Encodes the bitmap behind `bitmap` as PNG.
///
/// # Errors
/// Fails when GDI+ cannot start, cannot read the bitmap, or cannot encode it.
/// The result is checked to begin with a PNG signature, because an encoder
/// that reported success and wrote something else would otherwise be sent on
/// as an image.
pub fn bitmap_to_png(bitmap: Handle) -> Result<Vec<u8>, CliftError> {
    let _gdiplus = Gdiplus::start()?;
    let mut image = std::ptr::null_mut();
    // SAFETY: `bitmap` is a live HBITMAP owned by the clipboard; GDI+ copies
    // its pixels and does not take ownership. No palette.
    let status =
        unsafe { GdipCreateBitmapFromHBITMAP(bitmap, std::ptr::null_mut(), &raw mut image) };
    if status != 0 || image.is_null() {
        return Err(failed("GdipCreateBitmapFromHBITMAP", status));
    }
    let image = Image(image);
    let stream = Stream::new()?;
    // SAFETY: a live image, a live stream, the built-in encoder's id, and no
    // encoder parameters.
    let status = unsafe {
        GdipSaveImageToStream(image.0, stream.0, &raw const PNG_ENCODER, std::ptr::null())
    };
    if status != 0 {
        return Err(failed("GdipSaveImageToStream", status));
    }
    let bytes = stream.contents()?;
    if !bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Err(CliftError::new(
            Stage::Clipboard,
            ErrorKind::ClipboardRead,
            "GDI+ reported success but did not produce a PNG",
        ));
    }
    Ok(bytes)
}

/// Decodes PNG bytes into a GDI bitmap the clipboard can hold.
///
/// The returned handle is the caller's to give away or delete.
///
/// # Errors
/// Fails when GDI+ cannot start or the bytes are not an image it can read.
pub fn png_to_bitmap(bytes: &[u8]) -> Result<Handle, CliftError> {
    let _gdiplus = Gdiplus::start()?;
    let stream = Stream::over(bytes)?;
    let mut image = std::ptr::null_mut();
    // SAFETY: a live stream positioned at its start; GDI+ reads from it for
    // as long as the image lives, and `stream` outlives `image` below.
    let status = unsafe { GdipCreateBitmapFromStream(stream.0, &raw mut image) };
    if status != 0 || image.is_null() {
        return Err(failed("GdipCreateBitmapFromStream", status));
    }
    let image = Image(image);
    let mut bitmap = std::ptr::null_mut();
    // SAFETY: a live image; transparent pixels are composed onto white
    // (0xFFFFFFFF in ARGB), which is what a clipboard bitmap can express.
    let status = unsafe { GdipCreateHBITMAPFromBitmap(image.0, &raw mut bitmap, 0xFFFF_FFFF) };
    if status != 0 || bitmap.is_null() {
        return Err(failed("GdipCreateHBITMAPFromBitmap", status));
    }
    drop(image);
    drop(stream);
    Ok(bitmap)
}

fn failed(call: &str, status: i32) -> CliftError {
    CliftError::new(
        Stage::Clipboard,
        ErrorKind::ClipboardRead,
        format!("the clipboard image could not be converted: {call} returned {status}"),
    )
    .with_remedy(Remedy::new(
        "Save the screenshot as a file and send that instead:",
        "clift send <file>",
    ))
}
