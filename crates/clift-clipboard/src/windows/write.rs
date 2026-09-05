//! Putting text, or a redeemed attachment, on the clipboard.
//!
//! Two callers. `paste --copy` writes the insertion text -- a list of remote
//! paths, or a token. `fetch --copy` writes an attachment that came back from a
//! server, in **every form this build can offer it**: the file itself so that a
//! paste in Explorer produces it, the pixels so that an image editor takes
//! them, and text so that a cursor in a document gets something. All of them in
//! one clipboard session, because emptying the clipboard a second time would
//! throw away what the first pass wrote.

use super::gdiplus::{delete_bitmap, png_to_bitmap};
use super::session::{CF_BITMAP, CF_HDROP, CF_UNICODETEXT, Session, png_format, refused, wide};
use crate::drop_files;
use crate::offer::{Offer, Written};
use clift_core::error::CliftError;

/// Replaces the clipboard's contents with `text`.
///
/// # Errors
/// Fails when the clipboard cannot be opened or refuses the write, which is
/// reported rather than ignored: the user is about to be told their clipboard
/// was replaced, and that must be true.
pub fn write_text(text: &str) -> Result<(), CliftError> {
    let session = Session::open()?;
    session.empty()?;
    session.set_bytes(CF_UNICODETEXT, &utf16_bytes(text))
}

/// Offers one redeemed attachment to the clipboard in every form it has.
///
/// The bitmap is made **before** the clipboard is emptied. Decoding is the one
/// step here that can fail on the attachment's own contents, and doing it first
/// means a picture Windows will not decode leaves the clipboard exactly as it
/// was rather than emptied and half filled.
///
/// # Errors
/// Fails only when the clipboard cannot be opened or emptied, or when it takes
/// nothing at all. A format it refuses individually is reported in [`Written`]:
/// a paste that works in three applications out of four beats an error and an
/// untouched clipboard.
pub fn write_offer(offer: &Offer<'_>) -> Result<Written, CliftError> {
    let bitmap = match offer.image {
        // A refusal here is not fatal to the rest: the file and the text are
        // still worth putting on the clipboard.
        Some(bytes) => png_to_bitmap(bytes).ok(),
        None => None,
    };
    let hdrop = drop_files::payload(&[&offer.file.to_string_lossy()]);

    let session = match Session::open().and_then(|session| session.empty().map(|()| session)) {
        Ok(session) => session,
        Err(error) => {
            if let Some(bitmap) = bitmap {
                delete_bitmap(bitmap);
            }
            return Err(error);
        }
    };

    let file = match &hdrop {
        Some(payload) => session.set_bytes(CF_HDROP, payload).is_ok(),
        None => false,
    };

    let text = session
        .set_bytes(CF_UNICODETEXT, &utf16_bytes(offer.text))
        .is_ok();

    let mut image = false;
    if let Some(bitmap) = bitmap {
        if session.set_handle(CF_BITMAP, bitmap).is_ok() {
            image = true;
        } else {
            delete_bitmap(bitmap);
        }
        // The `PNG` format alongside the bitmap: a browser prefers it, an image
        // editor takes the bitmap. Offering one of the two is how a paste ends
        // up working in one application and not the next.
        if let Some(bytes) = offer.image
            && session.set_bytes(png_format(), bytes).is_ok()
        {
            image = true;
        }
    }

    if !file && !text && !image {
        return Err(refused(
            "the clipboard refused every form of the attachment",
        ));
    }
    Ok(Written { file, image, text })
}

fn utf16_bytes(text: &str) -> Vec<u8> {
    wide(text)
        .iter()
        .flat_map(|character| character.to_le_bytes())
        .collect()
}
