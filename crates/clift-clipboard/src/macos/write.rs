//! Putting things on the pasteboard.
//!
//! Two callers, both of them deliberate. `paste --copy` writes the insertion
//! text -- a list of remote paths, never an attachment's contents. `fetch
//! --copy` writes an attachment that has come back from a server the user asked
//! it to come back from.
//!
//! Neither writes anything unless the operation succeeded: replacing what
//! someone had copied and then failing would cost them twice.
//!
//! The second one writes **several representations of one thing**, which is how
//! a pasteboard is meant to be used: one `declareTypes` naming every type, then
//! the data for each. Finder takes the file URL, an image editor takes the PNG,
//! a cursor in a document takes the text. Declaring one type and hoping is what
//! made pasting a markdown file appear to do nothing.

use crate::macos::pasteboard::type_name;
use crate::offer::{Offer, Written};
use clift_core::error::{CliftError, ErrorKind, Stage};
use objc2_app_kit::NSPasteboard;
use objc2_foundation::{NSData, NSString, NSURL};

/// The three pasteboard types one attachment is offered as.
const FILE_URL: &str = "public.file-url";
const TEXT: &str = "public.utf8-plain-text";
const PNG: &str = "public.png";

/// Replaces the pasteboard's contents with `text`.
///
/// # Errors
/// Fails when the pasteboard refuses the write, which is reported rather than
/// ignored: the user is about to be told their clipboard was replaced, and that
/// must be true.
pub fn write_text(text: &str) -> Result<(), CliftError> {
    let pasteboard = NSPasteboard::generalPasteboard();
    pasteboard.clearContents();

    let types = objc2_foundation::NSArray::from_retained_slice(&[type_name(TEXT)]);
    // SAFETY: `owner` is nil, which is what AppKit expects when the caller
    // supplies the data immediately instead of promising it lazily. Passing an
    // owner would require it to outlive the pasteboard's interest in it; nil
    // has no lifetime to get wrong.
    unsafe { pasteboard.declareTypes_owner(&types, None) };

    let value = NSString::from_str(text);
    if pasteboard.setString_forType(&value, type_name(TEXT).as_ref()) {
        Ok(())
    } else {
        Err(CliftError::new(
            Stage::Clipboard,
            ErrorKind::ClipboardRead,
            "the clipboard refused the replacement text",
        ))
    }
}

/// Offers one redeemed attachment to the pasteboard in every form it has.
///
/// One `declareTypes` for all of them: declaring a second time clears what the
/// first one wrote, so this cannot be built out of one call per representation.
///
/// The image bytes are **not** this machine's own, which is the one thing worth
/// knowing here. Announcing something to every application on the system as
/// `public.png` is a claim, and the caller is required to have checked it --
/// `clift_core::universal::media::clipboard_form` is where that check lives,
/// and it looks at the signature rather than taking the sender's word.
///
/// # Errors
/// Fails only when the pasteboard takes nothing at all. A representation the
/// pasteboard refuses individually is reported in [`Written`], not raised: the
/// user is better served by a paste that works in three applications out of
/// four than by an error and an untouched clipboard.
pub fn write_offer(offer: &Offer<'_>) -> Result<Written, CliftError> {
    let file_url = NSURL::fileURLWithPath(&NSString::from_str(&offer.file.to_string_lossy()));
    let Some(url_text) = file_url.absoluteString() else {
        return Err(refused(
            "the attachment's path could not be made into a URL",
        ));
    };

    let mut names = vec![type_name(FILE_URL), type_name(TEXT)];
    if offer.image.is_some() {
        names.push(type_name(PNG));
    }

    let pasteboard = NSPasteboard::generalPasteboard();
    pasteboard.clearContents();
    let types = objc2_foundation::NSArray::from_retained_slice(&names);
    // SAFETY: `owner` is nil, which is what AppKit expects when the caller
    // supplies the data immediately instead of promising it lazily. Passing an
    // owner would require it to outlive the pasteboard's interest in it; nil
    // has no lifetime to get wrong.
    unsafe { pasteboard.declareTypes_owner(&types, None) };

    let file = pasteboard.setString_forType(&url_text, type_name(FILE_URL).as_ref());
    let text =
        pasteboard.setString_forType(&NSString::from_str(offer.text), type_name(TEXT).as_ref());
    let image = match offer.image {
        Some(bytes) => {
            pasteboard.setData_forType(Some(&NSData::with_bytes(bytes)), type_name(PNG).as_ref())
        }
        None => false,
    };

    if !file && !text && !image {
        return Err(refused(
            "the clipboard refused every form of the attachment",
        ));
    }
    Ok(Written { file, image, text })
}

fn refused(detail: &str) -> CliftError {
    CliftError::new(Stage::Clipboard, ErrorKind::ClipboardRead, detail)
}
