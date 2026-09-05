//! Choosing an image representation, and converting the one that needs it.
//!
//! A screenshot does not arrive as "an image". It arrives as several
//! representations of the same picture at once, and which one Clift takes is a
//! real decision: a macOS screen capture offers `public.png` **and**
//! `public.tiff`, and the TIFF is several times the size of the PNG of the same
//! pixels.
//!
//! So the order below is by what the receiving end gets, not by what is
//! convenient to read. TIFF is last and is converted rather than sent: an agent
//! asked to look at a screenshot should not be handed twelve megabytes of
//! uncompressed raster.

use clift_core::error::{CliftError, ErrorKind, Remedy, Stage};
use objc2_app_kit::{NSBitmapImageFileType, NSBitmapImageRep, NSPasteboard};
use objc2_foundation::{NSData, NSDictionary};

use crate::macos::pasteboard::type_name;

/// One clipboard image, in a format that can be written out as it stands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageData {
    pub mime: String,
    pub extension: &'static str,
    pub bytes: Vec<u8>,
}

/// The representations Clift takes as they are, best first.
///
/// PNG first because macOS offers it for screenshots and it is the smaller of
/// the two it offers. JPEG next: it is what a copy from a browser tends to be,
/// and re-encoding it would only lose more. WebP is here because if an
/// application ever offers one, passing the bytes through costs nothing --
/// Clift never decodes an image, so no decoder is needed to support it.
const DIRECT: [(&str, &str, &str); 3] = [
    ("public.png", "image/png", "png"),
    ("public.jpeg", "image/jpeg", "jpg"),
    ("org.webmproject.webp", "image/webp", "webp"),
];

/// The one representation Clift converts rather than forwards.
const TIFF: &str = "public.tiff";

/// Reads the best image representation the pasteboard offers.
///
/// # Errors
/// Fails when a representation is advertised but holds no data, and when a TIFF
/// cannot be converted. A failed conversion is reported rather than worked
/// around: the alternative would be sending a picture that is not the one the
/// user copied.
pub fn read_image(pasteboard: &NSPasteboard) -> Result<Option<ImageData>, CliftError> {
    for (identifier, mime, extension) in DIRECT {
        let Some(data) = pasteboard.dataForType(type_name(identifier).as_ref()) else {
            continue;
        };
        let bytes = data.to_vec();
        if bytes.is_empty() {
            return Err(empty(mime));
        }
        return Ok(Some(ImageData {
            mime: mime.to_string(),
            extension,
            bytes,
        }));
    }

    let Some(tiff) = pasteboard.dataForType(type_name(TIFF).as_ref()) else {
        return Ok(None);
    };
    if tiff.is_empty() {
        return Err(empty("image/tiff"));
    }
    Ok(Some(ImageData {
        mime: "image/png".to_string(),
        extension: "png",
        bytes: tiff_to_png(&tiff)?,
    }))
}

/// Re-encodes a TIFF as a PNG using the system imaging framework.
///
/// Lossless, and only lossless. Both formats hold exactly the pixels they were
/// given, so this changes the container and nothing else -- which is why a
/// failure here is reported instead of being papered over with a lossy
/// fallback: an attachment that is not what the user copied is worse than an
/// attachment that did not arrive.
///
/// No image crate is involved. `NSBitmapImageRep` is already on every macOS
/// machine, and a decoder in the binary would be both larger and one more thing
/// to keep patched.
fn tiff_to_png(tiff: &NSData) -> Result<Vec<u8>, CliftError> {
    let Some(representation) = NSBitmapImageRep::imageRepWithData(tiff) else {
        return Err(conversion_failed("macOS could not read the copied image"));
    };
    let properties = NSDictionary::new();
    let encoded = unsafe {
        representation.representationUsingType_properties(NSBitmapImageFileType::PNG, &properties)
    };
    let Some(encoded) = encoded else {
        return Err(conversion_failed(
            "macOS could not re-encode the copied image as a PNG",
        ));
    };

    let bytes = encoded.to_vec();
    if bytes.is_empty() {
        return Err(conversion_failed("the re-encoded image came out empty"));
    }
    Ok(bytes)
}

fn empty(mime: &str) -> CliftError {
    CliftError::new(
        Stage::Clipboard,
        ErrorKind::ClipboardRead,
        format!("the clipboard offered a {mime} image with no data in it"),
    )
}

fn conversion_failed(detail: &str) -> CliftError {
    CliftError::new(
        Stage::Clipboard,
        ErrorKind::ClipboardRead,
        detail.to_string(),
    )
    .with_remedy(Remedy::new(
        "Save the image to a file and send that instead:",
        "clift send <file>",
    ))
}
