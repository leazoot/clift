//! Choosing an image representation, and converting the one that needs it.
//!
//! The order is by what the receiving end gets. PNG bytes an application put
//! on the clipboard itself are taken as they are. Otherwise the picture is on
//! the clipboard as a GDI bitmap, which is what the system screenshot tools
//! produce, and it is encoded as PNG here rather than sent as raw pixels.

use super::gdiplus::bitmap_to_png;
use super::session::{CF_BITMAP, Session, png_format};
use clift_core::error::{CliftError, ErrorKind, Stage};

/// One clipboard image, as PNG.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageData {
    pub mime: &'static str,
    pub extension: &'static str,
    pub bytes: Vec<u8>,
}

/// Reads the best image representation the clipboard offers.
///
/// `CF_BITMAP` is asked for last and covers every bitmap form: when an
/// application puts a device independent bitmap on the clipboard, Windows
/// synthesises the `CF_BITMAP` handle on request.
///
/// # Errors
/// Fails when a representation is advertised but empty, and when the bitmap
/// cannot be converted. A failed conversion is reported rather than worked
/// around: the alternative would be sending a picture that is not the one
/// the user copied.
pub fn read_image(session: &Session) -> Result<Option<ImageData>, CliftError> {
    if let Some(bytes) = session.bytes(png_format())? {
        if bytes.is_empty() {
            return Err(empty("PNG"));
        }
        return Ok(Some(png(bytes)));
    }

    let Some(bitmap) = session.handle(CF_BITMAP) else {
        return Ok(None);
    };
    Ok(Some(png(bitmap_to_png(bitmap)?)))
}

fn png(bytes: Vec<u8>) -> ImageData {
    ImageData {
        mime: "image/png",
        extension: "png",
        bytes,
    }
}

fn empty(format: &str) -> CliftError {
    CliftError::new(
        Stage::Clipboard,
        ErrorKind::ClipboardRead,
        format!("the clipboard offers {format} but holds no data for it"),
    )
}
