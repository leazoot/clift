//! Which media types Universal Mode will carry, and the agreement between a
//! media type and a file name.
//!
//! Fast Mode has no such list and should not grow one: it uploads whatever the
//! user pointed at, over their own SSH, to their own machine, and Clift never
//! opens it. Universal Mode is different in one respect that justifies the
//! difference in policy -- the ciphertext passes through a relay the user may
//! not own, and `clift fetch` writes the result onto the remote host without a
//! person looking at it first. Narrowing what can make that trip to the things
//! an agent is actually asked to look at costs the user nothing real and takes
//! "Clift wrote an executable into your home directory" off the table.
//!
//! This is not a security boundary against a determined sender: on the way to a
//! file the bytes are never inspected, so anything can be renamed to `.png`. It
//! is a boundary against accident, and it is stated as such rather than dressed
//! up. [`goes_on_the_clipboard`] is the one place that does look at the bytes,
//! and it says why.

use crate::domain::SafeFileName;
use crate::error::{CliftError, ErrorKind, Remedy, Stage};

/// Media type, and the extensions that may accompany it.
///
/// The first extension in each row is the canonical one, used when a name has
/// none at all.
const CARRIED: &[(&str, &[&str])] = &[
    ("image/png", &["png"]),
    ("image/jpeg", &["jpg", "jpeg"]),
    ("image/gif", &["gif"]),
    ("image/webp", &["webp"]),
    ("image/tiff", &["tif", "tiff"]),
    ("image/heic", &["heic"]),
    ("image/svg+xml", &["svg"]),
    ("application/pdf", &["pdf"]),
    ("text/plain", &["txt", "log", "text"]),
    ("text/markdown", &["md", "markdown"]),
    ("text/csv", &["csv"]),
    ("application/json", &["json"]),
];

/// Every media type Universal Mode carries, for help text and documentation.
#[must_use]
pub fn carried_types() -> Vec<&'static str> {
    CARRIED.iter().map(|(media_type, _)| *media_type).collect()
}

/// The media type a file name suggests, if it suggests one this build carries.
#[must_use]
pub fn from_extension(name: &SafeFileName) -> Option<&'static str> {
    let extension = name
        .extension()?
        .trim_start_matches('.')
        .to_ascii_lowercase();
    CARRIED
        .iter()
        .find(|(_, extensions)| extensions.contains(&extension.as_str()))
        .map(|(media_type, _)| *media_type)
}

/// The eight bytes every PNG begins with.
const PNG_SIGNATURE: &[u8] = &[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];

/// Text types this build will hand to a cursor in a document.
///
/// `image/svg+xml` is text and is deliberately not here: it is carried as a
/// picture, and pasting its source into someone's prose is not what they asked
/// for when they copied an image.
const AS_TEXT: &[&str] = &[
    "text/plain",
    "text/markdown",
    "text/csv",
    "application/json",
];

/// How a redeemed attachment should be offered to this machine's clipboard.
///
/// A clipboard holds **several representations of one thing at once**, and the
/// application doing the pasting picks the one it understands. That is the
/// whole reason this is not a yes-or-no question. A file manager wants a file
/// reference; an image editor wants pixels; a cursor in a document wants text.
///
/// Offering only one of the three is what made pasting a markdown file appear
/// to do nothing at all: the file arrived, its path was printed to a log nobody
/// reads, and the clipboard was deliberately left alone.
///
/// Every form includes the file reference. What varies is what else goes
/// alongside it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClipboardForm {
    /// A real PNG: the pixels, so an editor or a chat window can take them.
    Image,
    /// Text this build can hand to a cursor: the content itself.
    Text,
    /// Anything else. The path goes on as text, so a paste is never silent.
    FileOnly,
}

/// Decides [`ClipboardForm`] for one attachment.
///
/// The bytes are looked at, which nothing else in this module does. For an
/// image that is a safety check -- announcing bytes to every application on the
/// machine as `public.png` is a claim, and in this direction the bytes came
/// from a machine the user is not sitting at, so the claim is verified rather
/// than taken from the sender. For text it is a correctness check: a file
/// declared `text/plain` that is not valid UTF-8 has no text to offer.
#[must_use]
pub fn clipboard_form(media_type: &str, bytes: &[u8]) -> ClipboardForm {
    let normalised = media_type.trim().to_ascii_lowercase();
    if normalised == "image/png" {
        return if bytes.starts_with(PNG_SIGNATURE) {
            ClipboardForm::Image
        } else {
            // Said it was a PNG and is not. The file still lands under its own
            // name; what it does not get is a claim this build cannot stand
            // behind.
            ClipboardForm::FileOnly
        };
    }
    if AS_TEXT.contains(&normalised.as_str()) && std::str::from_utf8(bytes).is_ok() {
        return ClipboardForm::Text;
    }
    ClipboardForm::FileOnly
}

/// Checks that `media_type` is carried, and that `name` does not contradict it.
///
/// A name with no extension is accepted: the clipboard hands over images with
/// names Clift invented, and refusing them would refuse the main use case. A
/// name with an extension that belongs to a *different* carried type is
/// refused, because that combination cannot arise by accident and the far side
/// writes the file under the name, not under the media type.
///
/// # Errors
/// Returns [`ErrorKind::LimitExceeded`] for a type this build does not carry,
/// and [`ErrorKind::IntegrityFailure`] when the name and the type disagree.
pub fn check(media_type: &str, name: &SafeFileName) -> Result<(), CliftError> {
    let normalised = media_type.trim().to_ascii_lowercase();
    let Some((_, extensions)) = CARRIED
        .iter()
        .find(|(carried, _)| *carried == normalised.as_str())
    else {
        return Err(CliftError::new(
            Stage::Relay,
            ErrorKind::LimitExceeded,
            format!("Universal Mode does not carry {media_type:?}"),
        )
        .with_remedy(Remedy::new(
            "Send it over your own SSH connection instead:",
            "clift send <file> --to <target>",
        )));
    };

    let Some(extension) = name.extension() else {
        return Ok(());
    };
    let extension = extension.trim_start_matches('.').to_ascii_lowercase();
    if extensions.contains(&extension.as_str()) {
        return Ok(());
    }
    // An extension nothing carries is not a contradiction, only an oddity: the
    // user may have copied `notes.bak`. What is refused is `report.pdf`
    // declared as `image/png`, which nothing legitimate produces.
    if from_extension(name).is_none() {
        return Ok(());
    }

    Err(CliftError::new(
        Stage::Relay,
        ErrorKind::IntegrityFailure,
        format!(
            "{:?} is declared as {media_type:?}, which its name contradicts",
            name.as_str()
        ),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn name(value: &str) -> SafeFileName {
        SafeFileName::new(value).unwrap_or_else(|error| panic!("{error}"))
    }

    #[test]
    fn the_types_the_clipboard_produces_are_carried() {
        for (media_type, extension) in [
            ("image/png", "shot.png"),
            ("image/jpeg", "photo.JPG"),
            ("image/tiff", "scan.tiff"),
            ("application/pdf", "spec.pdf"),
            ("text/markdown", "notes.md"),
        ] {
            check(media_type, &name(extension)).unwrap_or_else(|error| panic!("{error}"));
        }
    }

    #[test]
    fn an_uncarried_type_is_refused_with_a_way_out() {
        let error = check("application/x-sh", &name("run.sh")).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::LimitExceeded);
        assert!(
            error
                .remedy()
                .is_some_and(|r| r.command().contains("clift send"))
        );
    }

    #[test]
    fn a_name_that_contradicts_its_type_is_refused() {
        let error = check("image/png", &name("report.pdf")).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::IntegrityFailure);
    }

    #[test]
    fn a_name_without_a_useful_extension_is_accepted() {
        check("image/png", &name("clipboard")).unwrap_or_else(|error| panic!("{error}"));
        check("image/png", &name("archive.bak")).unwrap_or_else(|error| panic!("{error}"));
    }

    #[test]
    fn an_extension_maps_back_to_its_type() {
        assert_eq!(from_extension(&name("a.PNG")), Some("image/png"));
        assert_eq!(from_extension(&name("a.jpeg")), Some("image/jpeg"));
        assert_eq!(from_extension(&name("a.bin")), None);
        assert_eq!(from_extension(&name("a")), None);
    }

    #[test]
    fn each_attachment_gets_the_representations_it_can_actually_offer() {
        let png = [
            &[0x89_u8, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A][..],
            &[0x00, 0x00, 0x00, 0x0D][..],
        ]
        .concat();

        assert_eq!(clipboard_form("image/png", &png), ClipboardForm::Image);
        assert_eq!(clipboard_form("image/PNG", &png), ClipboardForm::Image);

        // The case this looks at bytes for: it says PNG and is not one. The
        // attachment still lands; it just does not get announced as a picture.
        assert_eq!(
            clipboard_form("image/png", b"<html>not a picture"),
            ClipboardForm::FileOnly
        );
        assert_eq!(
            clipboard_form("image/png", &png[..4]),
            ClipboardForm::FileOnly
        );
        assert_eq!(clipboard_form("image/png", b""), ClipboardForm::FileOnly);

        // Text a cursor can take.
        for text_type in AS_TEXT {
            assert_eq!(
                clipboard_form(text_type, "# heading\n".as_bytes()),
                ClipboardForm::Text,
                "{text_type}"
            );
        }
        // Declared text, not valid UTF-8: there is no text to offer.
        assert_eq!(
            clipboard_form("text/plain", &[0xFF, 0xFE, 0x00]),
            ClipboardForm::FileOnly
        );

        // Carried, but neither a picture this build writes nor text.
        assert_eq!(
            clipboard_form("application/pdf", b"%PDF-1.7"),
            ClipboardForm::FileOnly
        );
        assert_eq!(clipboard_form("image/jpeg", &png), ClipboardForm::FileOnly);
        // Deliberately not text: it is carried as a picture.
        assert_eq!(
            clipboard_form("image/svg+xml", b"<svg/>"),
            ClipboardForm::FileOnly
        );
    }

    #[test]
    fn no_carried_type_could_be_mistaken_for_an_executable_or_an_archive() {
        for (media_type, _) in CARRIED {
            for forbidden in [
                "x-sh",
                "x-executable",
                "zip",
                "gzip",
                "x-tar",
                "octet-stream",
            ] {
                assert!(!media_type.contains(forbidden), "{media_type} is carried");
            }
        }
    }
}
