//! Where the attachments of one send come from.
//!
//! The specification fixes the order, and the order is the whole of this module:
//!
//! 1. files named on the command line;
//! 2. files on the clipboard;
//! 3. an image on the clipboard;
//! 4. plain text: nothing to send, and the terminal does its own paste;
//! 5. nothing at all.
//!
//! Two things about it are not obvious. **A file list beats an image**: macOS
//! puts an icon on the pasteboard alongside a copied file, so a selection of
//! files also looks like an image, and sending the icon instead of the files
//! would be absurd. And **plain text is resolved before anything connects**:
//! that is what makes an ordinary paste cost nothing, so this function
//! is given no transport at all rather than being trusted not to use one.

use crate::attachments::inspect_all;
use crate::domain::{FileKind, LocalAttachment, SafeFileName};
use crate::error::{CliftError, ErrorKind, Stage};
use crate::ports::{ClipboardSnapshot, ClipboardSource};
use std::path::PathBuf;

/// Which of the five cases a send turned out to be.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    ExplicitFiles,
    ClipboardFiles,
    ClipboardImage,
}

/// The attachments to send, and where they came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolved {
    origin: Origin,
    attachments: Vec<LocalAttachment>,
}

impl Resolved {
    #[must_use]
    pub const fn origin(&self) -> Origin {
        self.origin
    }

    #[must_use]
    pub fn attachments(&self) -> &[LocalAttachment] {
        &self.attachments
    }
}

/// Decides what this send is about, without touching the network.
///
/// The clipboard is read at most once, and only when no files were named: a
/// user who typed a file list has already said what they meant, and reading
/// their clipboard anyway would be both pointless and intrusive.
///
/// # Errors
/// Returns exit code 10 when there is nothing to send, which is a normal
/// outcome rather than a malfunction: the terminal integration turns it into an
/// ordinary paste. Returns a clipboard error when the clipboard cannot be read,
/// and a validation error when a named file is not something Clift can send.
pub fn resolve(
    explicit: &[PathBuf],
    clipboard: Option<&dyn ClipboardSource>,
) -> Result<Resolved, CliftError> {
    if !explicit.is_empty() {
        return Ok(Resolved {
            origin: Origin::ExplicitFiles,
            attachments: inspect_all(explicit)?,
        });
    }

    let Some(clipboard) = clipboard else {
        return Err(nothing(
            "no files were given and there is no clipboard here",
        ));
    };

    let snapshot = clipboard.read_snapshot()?;
    from_clipboard(&snapshot)
}

fn from_clipboard(snapshot: &ClipboardSnapshot) -> Result<Resolved, CliftError> {
    if !snapshot.files.is_empty() {
        return Ok(Resolved {
            origin: Origin::ClipboardFiles,
            attachments: inspect_all(&snapshot.files)?,
        });
    }

    if !snapshot.images.is_empty() {
        let mut attachments = Vec::with_capacity(snapshot.images.len());
        for (index, image) in snapshot.images.iter().enumerate() {
            let size = std::fs::metadata(&image.path)
                .map_err(|error| {
                    CliftError::new(
                        Stage::Clipboard,
                        ErrorKind::ClipboardRead,
                        "the image taken from the clipboard could not be read back",
                    )
                    .with_source(error)
                })?
                .len();
            attachments.push(
                LocalAttachment::new(
                    image.path.clone(),
                    image_name(&image.path, index),
                    size,
                    FileKind::Regular,
                )
                .map_err(|error| error.into_clift(Stage::Clipboard, ErrorKind::ClipboardRead))?,
            );
        }
        return Ok(Resolved {
            origin: Origin::ClipboardImage,
            attachments,
        });
    }

    if snapshot.text.is_some() {
        // Not a failure. The terminal integration reads this exit code and
        // performs its own paste, which is how plain text stays byte for byte
        // what it was.
        return Err(nothing("the clipboard holds text, which needs no upload"));
    }

    Err(nothing("the clipboard is empty"))
}

/// The name a clipboard image is given on the far side.
///
/// A copied image has no name of its own, and the temporary file's name is an
/// implementation detail nobody wants pasted into their agent's prompt. The
/// extension is kept because it is the one thing about the name that carries
/// information.
fn image_name(path: &std::path::Path, index: usize) -> SafeFileName {
    let extension = path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or("png");
    let stem = if index == 0 {
        "clipboard".to_string()
    } else {
        format!("clipboard-{}", index + 1)
    };
    SafeFileName::sanitize(&format!("{stem}.{extension}"))
}

fn nothing(detail: &str) -> CliftError {
    CliftError::new(
        Stage::Clipboard,
        ErrorKind::NoAttachment,
        format!("nothing to send: {detail}"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ports::{ClipboardImage, RemoteFs, TransportTarget};
    use crate::testing::{FakeClipboard, RecordingTransport};
    use std::path::Path;

    /// Uniqueness without asking the operating system for a process id. The
    /// architecture check forbids any use of the process module anywhere in
    /// this crate, and a counter answers the question just as well.
    static SCRATCH: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

    struct Scratch(PathBuf);

    impl Scratch {
        fn new(label: &str) -> Self {
            let unique = SCRATCH.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!("clift-source-{label}-{unique}"));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir_all(&path).unwrap_or_else(|error| panic!("{error}"));
            Self(path)
        }

        fn file(&self, name: &str) -> PathBuf {
            let path = self.0.join(name);
            std::fs::write(&path, b"contents").unwrap_or_else(|error| panic!("{error}"));
            path
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn snapshot(
        text: Option<&str>,
        files: Vec<PathBuf>,
        images: Vec<PathBuf>,
    ) -> ClipboardSnapshot {
        ClipboardSnapshot {
            text: text.map(str::to_string),
            files,
            images: images
                .into_iter()
                .map(|path| ClipboardImage {
                    mime: "image/png".to_string(),
                    path,
                })
                .collect(),
        }
    }

    /// The whole of the specification, one row per case.
    #[test]
    fn the_five_cases_are_decided_in_the_order_fr_014_gives() {
        let scratch = Scratch::new("priority");
        let explicit = scratch.file("named.png");
        let copied = scratch.file("copied.txt");
        let image = scratch.file("image.png");

        // 1. Named files win over everything else on the clipboard.
        let clipboard = FakeClipboard::with(snapshot(
            Some("some text"),
            vec![copied.clone()],
            vec![image.clone()],
        ));
        let resolved = resolve(std::slice::from_ref(&explicit), Some(&clipboard)).unwrap();
        assert_eq!(resolved.origin(), Origin::ExplicitFiles);
        assert_eq!(resolved.attachments()[0].name().as_str(), "named.png");
        assert_eq!(
            clipboard.reads(),
            0,
            "a user who named files has already said what they meant"
        );

        // 2. A file list beats an image: copying a file in Finder also puts its
        // icon on the pasteboard, and the icon is not what was copied.
        let clipboard = FakeClipboard::with(snapshot(
            Some("some text"),
            vec![copied.clone()],
            vec![image.clone()],
        ));
        let resolved = resolve(&[], Some(&clipboard)).unwrap();
        assert_eq!(resolved.origin(), Origin::ClipboardFiles);
        assert_eq!(resolved.attachments()[0].name().as_str(), "copied.txt");

        // 3. An image, when there is no file list.
        let clipboard =
            FakeClipboard::with(snapshot(Some("some text"), vec![], vec![image.clone()]));
        let resolved = resolve(&[], Some(&clipboard)).unwrap();
        assert_eq!(resolved.origin(), Origin::ClipboardImage);
        assert_eq!(resolved.attachments()[0].name().as_str(), "clipboard.png");

        // 4. Plain text: nothing to send.
        let clipboard = FakeClipboard::with(snapshot(Some("just words"), vec![], vec![]));
        let error = resolve(&[], Some(&clipboard)).expect_err("text is not an attachment");
        assert_eq!(error.exit_code().as_u8(), 10);
        assert!(error.to_string().contains("text"), "{error}");

        // 5. An empty clipboard: also nothing, and it says so differently.
        let clipboard = FakeClipboard::with(snapshot(None, vec![], vec![]));
        let error = resolve(&[], Some(&clipboard)).expect_err("an empty clipboard sends nothing");
        assert_eq!(error.exit_code().as_u8(), 10);
        assert!(error.to_string().contains("empty"), "{error}");
    }

    /// The plain-text rule and the plain-text rule: an ordinary paste must not open a connection.
    ///
    /// The composition below is the one `send` performs. The transport is in
    /// scope and would be used if the text case reached it; the assertion is
    /// that it never does.
    #[test]
    fn a_plain_text_clipboard_opens_no_connection_at_all() {
        let transport = RecordingTransport::new("/home/dev");
        let clipboard = FakeClipboard::with(snapshot(Some("just words"), vec![], vec![]));

        let outcome = resolve(&[], Some(&clipboard)).and_then(|resolved| {
            // Whatever a send would do first. Never reached for plain text.
            transport.probe(&TransportTarget::new("core"))?;
            Ok(resolved)
        });

        assert!(outcome.is_err());
        assert_eq!(
            transport.call_count(),
            0,
            "plain text must cost nothing: not one round trip"
        );
        assert_eq!(
            clipboard.reads(),
            1,
            "the clipboard is read once, not polled"
        );
    }

    #[test]
    fn several_clipboard_images_get_distinct_names() {
        let scratch = Scratch::new("images");
        let first = scratch.file("a.png");
        let second = scratch.file("b.png");
        let clipboard = FakeClipboard::with(snapshot(None, vec![], vec![first, second]));

        let resolved = resolve(&[], Some(&clipboard)).unwrap();
        let names: Vec<&str> = resolved
            .attachments()
            .iter()
            .map(|attachment| attachment.name().as_str())
            .collect();
        assert_eq!(names, ["clipboard.png", "clipboard-2.png"]);
    }

    #[test]
    fn a_named_file_that_cannot_be_sent_is_refused_rather_than_skipped() {
        let scratch = Scratch::new("bad");
        let good = scratch.file("fine.png");
        let missing = scratch.0.join("not-there.png");

        let error = resolve(&[good, missing], None).expect_err("one path is not there");
        assert_eq!(error.exit_code().as_u8(), 24);
    }

    #[test]
    fn no_files_and_no_clipboard_is_nothing_to_send() {
        let error = resolve(&[], None).expect_err("there is no source at all");
        assert_eq!(error.exit_code().as_u8(), 10);
    }

    #[test]
    fn an_image_keeps_the_extension_of_what_was_copied() {
        assert_eq!(
            image_name(Path::new("/run/.clipboard.0.jpg"), 0).as_str(),
            "clipboard.jpg"
        );
        assert_eq!(
            image_name(Path::new("/run/.clipboard.0.png"), 0).as_str(),
            "clipboard.png"
        );
        // A path with no extension at all falls back to png rather than
        // producing a name with nothing after the dot.
        assert_eq!(
            image_name(Path::new("/run/clipboard"), 0).as_str(),
            "clipboard.png"
        );
    }
}
