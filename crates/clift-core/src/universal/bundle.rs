//! What is inside a sealed object: the attachments, with just enough about each
//! one for the far side to write it down.
//!
//! Hand framed rather than JSON. Two reasons, and the second is the real one:
//!
//! - the frame carries raw bytes, so a text format would need base64 and a
//!   third of the payload again;
//! - the decoder is the first thing that touches attacker-influenced bytes
//!   after the AEAD, and a fixed-width frame with explicit lengths is a decoder
//!   whose failure modes can be enumerated in a test. A general parser is not.
//!
//! ```text
//! u8   entry count            1..=MAX_ENTRIES
//! per entry, in order:
//!   u16  name length          1..=255,   UTF-8, must survive SafeFileName
//!   ..   name
//!   u16  media type length    1..=127,   ASCII
//!   ..   media type
//!   u64  data length
//!   ..   data
//! ```
//!
//! All integers are big endian. Nothing here is optional and nothing is
//! skipped: trailing bytes after the last entry are a decode failure, because
//! the only way to produce them is for something to have gone wrong.

use crate::domain::{MAX_FILE_NAME_LEN, SafeFileName};
use crate::error::{CliftError, ErrorKind, Stage};
use crate::universal::media;

/// Attachments in one object. The same ceiling as a Fast Mode batch
/// (`Limits::max_files`), so the two modes refuse the same selection.
pub const MAX_ENTRIES: usize = 20;

/// Longest media type string accepted. Real ones are far shorter; this exists
/// so a malformed frame cannot ask for a large allocation.
const MAX_MEDIA_TYPE_LEN: usize = 127;

/// One attachment inside a sealed object.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BundleEntry {
    name: SafeFileName,
    media_type: String,
    data: Vec<u8>,
}

impl BundleEntry {
    /// # Errors
    /// Fails when the media type is not one Universal Mode carries, or does not
    /// agree with the file name's extension.
    pub fn new(
        name: SafeFileName,
        media_type: impl Into<String>,
        data: Vec<u8>,
    ) -> Result<Self, CliftError> {
        let media_type = media_type.into();
        media::check(&media_type, &name)?;
        Ok(Self {
            name,
            media_type,
            data,
        })
    }

    #[must_use]
    pub const fn name(&self) -> &SafeFileName {
        &self.name
    }

    #[must_use]
    pub fn media_type(&self) -> &str {
        &self.media_type
    }

    #[must_use]
    pub fn data(&self) -> &[u8] {
        &self.data
    }
}

/// Serialises the entries into the frame a sealed object carries.
///
/// # Errors
/// Fails when there are no entries, too many of them, or a name or media type
/// is longer than the frame can express.
pub fn encode(entries: &[BundleEntry]) -> Result<Vec<u8>, CliftError> {
    if entries.is_empty() {
        return Err(malformed("there is nothing to send"));
    }
    if entries.len() > MAX_ENTRIES {
        return Err(CliftError::new(
            Stage::Relay,
            ErrorKind::LimitExceeded,
            format!(
                "{} attachments is more than the {MAX_ENTRIES} one object can carry",
                entries.len()
            ),
        ));
    }

    let capacity: usize = entries
        .iter()
        .map(|entry| entry.name.as_str().len() + entry.media_type.len() + entry.data.len() + 13)
        .sum();
    let mut frame = Vec::with_capacity(capacity + 1);

    // The count fits in a u8 because MAX_ENTRIES is 20; the check above is what
    // makes this cast total rather than a hope.
    frame.push(u8::try_from(entries.len()).unwrap_or(u8::MAX));
    for entry in entries {
        push_short(&mut frame, entry.name.as_str().as_bytes(), "file name")?;
        push_short(&mut frame, entry.media_type.as_bytes(), "media type")?;
        frame.extend_from_slice(&(entry.data.len() as u64).to_be_bytes());
        frame.extend_from_slice(&entry.data);
    }
    Ok(frame)
}

fn push_short(frame: &mut Vec<u8>, bytes: &[u8], what: &str) -> Result<(), CliftError> {
    let length = u16::try_from(bytes.len())
        .map_err(|error| malformed(format!("the {what} is too long to send")).with_source(error))?;
    frame.extend_from_slice(&length.to_be_bytes());
    frame.extend_from_slice(bytes);
    Ok(())
}

/// Reads the frame back, checking every length before it is used.
///
/// # Errors
/// Fails on a truncated frame, a count outside the permitted range, a name that
/// is not a [`SafeFileName`], a media type Universal Mode does not carry, and
/// on any trailing bytes.
pub fn decode(frame: &[u8]) -> Result<Vec<BundleEntry>, CliftError> {
    let mut reader = Reader::new(frame);
    let count = usize::from(reader.u8()?);
    if count == 0 {
        return Err(malformed("the object says it holds no attachments"));
    }
    if count > MAX_ENTRIES {
        return Err(CliftError::new(
            Stage::Relay,
            ErrorKind::LimitExceeded,
            format!("the object claims {count} attachments, more than the {MAX_ENTRIES} allowed"),
        ));
    }

    let mut entries = Vec::with_capacity(count);
    for index in 0..count {
        let name = reader.short_text(MAX_FILE_NAME_LEN, "file name")?;
        let media_type = reader.short_text(MAX_MEDIA_TYPE_LEN, "media type")?;
        let length = reader.u64()?;
        let length = usize::try_from(length).map_err(|error| {
            malformed(format!(
                "attachment {} declares an impossible size",
                index + 1
            ))
            .with_source(error)
        })?;
        let data = reader.take(length)?.to_vec();

        // `SafeFileName::new` rather than `sanitize`: the sender already
        // sanitised it, so a name that needs cleaning here means the frame was
        // built by something other than Clift, and repairing it quietly is how
        // a path traversal becomes a warning nobody reads.
        let name = SafeFileName::new(name)
            .map_err(|error| error.into_clift(Stage::Relay, ErrorKind::IntegrityFailure))?;
        entries.push(BundleEntry::new(name, media_type, data)?);
    }

    if !reader.is_empty() {
        return Err(malformed(format!(
            "the object has {} unexpected trailing bytes",
            reader.remaining()
        )));
    }
    Ok(entries)
}

/// A bounds-checked cursor. Every read either yields exactly what it promised
/// or fails; there is no partial read and no silent zero fill.
struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    const fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, at: 0 }
    }

    const fn remaining(&self) -> usize {
        self.bytes.len() - self.at
    }

    const fn is_empty(&self) -> bool {
        self.remaining() == 0
    }

    fn take(&mut self, count: usize) -> Result<&'a [u8], CliftError> {
        let end = self.at.checked_add(count).ok_or_else(|| {
            malformed("the object declares a length that overflows its own frame")
        })?;
        if end > self.bytes.len() {
            return Err(malformed(format!(
                "the object is truncated: {count} more bytes were needed and {} remain",
                self.remaining()
            )));
        }
        let slice = &self.bytes[self.at..end];
        self.at = end;
        Ok(slice)
    }

    fn u8(&mut self) -> Result<u8, CliftError> {
        Ok(self.take(1)?[0])
    }

    fn u16(&mut self) -> Result<u16, CliftError> {
        let bytes = self.take(2)?;
        Ok(u16::from_be_bytes([bytes[0], bytes[1]]))
    }

    fn u64(&mut self) -> Result<u64, CliftError> {
        let bytes = self.take(8)?;
        let mut array = [0_u8; 8];
        array.copy_from_slice(bytes);
        Ok(u64::from_be_bytes(array))
    }

    fn short_text(&mut self, limit: usize, what: &str) -> Result<String, CliftError> {
        let length = usize::from(self.u16()?);
        if length == 0 {
            return Err(malformed(format!("the object has an empty {what}")));
        }
        if length > limit {
            return Err(malformed(format!(
                "the object has a {what} of {length} bytes, over the {limit} allowed"
            )));
        }
        let bytes = self.take(length)?;
        String::from_utf8(bytes.to_vec()).map_err(|error| {
            malformed(format!("the object's {what} is not UTF-8")).with_source(error)
        })
    }
}

fn malformed(message: impl Into<String>) -> CliftError {
    CliftError::new(Stage::Relay, ErrorKind::IntegrityFailure, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &str, media_type: &str, data: &[u8]) -> BundleEntry {
        BundleEntry::new(
            SafeFileName::new(name).unwrap_or_else(|error| panic!("{error}")),
            media_type,
            data.to_vec(),
        )
        .unwrap_or_else(|error| panic!("{error}"))
    }

    #[test]
    fn one_entry_round_trips() {
        let entries = vec![entry("shot.png", "image/png", b"\x89PNG-ish")];
        let decoded = decode(&encode(&entries).unwrap_or_else(|e| panic!("{e}")))
            .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(decoded, entries);
    }

    #[test]
    fn several_entries_keep_their_order_and_their_bytes() {
        let entries = vec![
            entry("a.png", "image/png", b"first"),
            entry("b.pdf", "application/pdf", b"second"),
            entry("c.txt", "text/plain", b""),
        ];
        let decoded = decode(&encode(&entries).unwrap_or_else(|e| panic!("{e}")))
            .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(decoded, entries);
        assert_eq!(decoded[2].data(), b"");
    }

    #[test]
    fn an_empty_bundle_is_refused_on_both_sides() {
        assert!(encode(&[]).is_err());
        assert!(decode(&[0]).is_err());
    }

    #[test]
    fn too_many_entries_are_refused_on_both_sides() {
        let entries: Vec<BundleEntry> = (0..=MAX_ENTRIES)
            .map(|index| entry(&format!("f{index}.png"), "image/png", b"x"))
            .collect();
        let error = encode(&entries).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::LimitExceeded);

        // And a frame that merely *claims* too many is refused before the
        // claimed count is used to allocate anything.
        let mut frame = vec![0_u8; 1];
        frame[0] = 250;
        assert_eq!(decode(&frame).unwrap_err().kind(), ErrorKind::LimitExceeded);
    }

    /// Every prefix of a valid frame must fail. This is the assertion that the
    /// reader never fills the rest in with zeroes.
    #[test]
    fn every_truncation_is_caught() {
        let frame = encode(&[entry("shot.png", "image/png", b"payload")])
            .unwrap_or_else(|error| panic!("{error}"));
        for length in 0..frame.len() {
            assert!(
                decode(&frame[..length]).is_err(),
                "a {length}-byte prefix decoded"
            );
        }
    }

    #[test]
    fn trailing_bytes_are_refused_rather_than_ignored() {
        let mut frame = encode(&[entry("shot.png", "image/png", b"payload")])
            .unwrap_or_else(|error| panic!("{error}"));
        frame.push(0);
        let error = decode(&frame).unwrap_err();
        assert!(error.message().contains("trailing"), "{}", error.message());
    }

    /// A frame that says "this attachment is 2^60 bytes" must fail on the
    /// length check, not on an allocation.
    #[test]
    fn an_absurd_declared_length_fails_without_allocating() {
        let mut frame = vec![1_u8];
        frame.extend_from_slice(&8_u16.to_be_bytes());
        frame.extend_from_slice(b"shot.png");
        frame.extend_from_slice(&9_u16.to_be_bytes());
        frame.extend_from_slice(b"image/png");
        frame.extend_from_slice(&(1_u64 << 60).to_be_bytes());
        let error = decode(&frame).unwrap_err();
        assert!(error.message().contains("truncated"), "{}", error.message());
    }

    /// The decoder must not accept a name a sender could not have produced.
    /// This is the traversal check at the frame layer, before any path is built.
    #[test]
    fn a_name_that_is_not_a_safe_file_name_is_refused() {
        for name in ["../etc/passwd", "a/b.png", "..", "-rf.png", "\u{7f}.png"] {
            let mut frame = vec![1_u8];
            let bytes = name.as_bytes();
            frame.extend_from_slice(&(bytes.len() as u16).to_be_bytes());
            frame.extend_from_slice(bytes);
            frame.extend_from_slice(&9_u16.to_be_bytes());
            frame.extend_from_slice(b"image/png");
            frame.extend_from_slice(&0_u64.to_be_bytes());
            assert!(decode(&frame).is_err(), "accepted the name {name:?}");
        }
    }

    #[test]
    fn a_media_type_that_is_not_carried_is_refused() {
        let mut frame = vec![1_u8];
        frame.extend_from_slice(&5_u16.to_be_bytes());
        frame.extend_from_slice(b"x.png");
        frame.extend_from_slice(&15_u16.to_be_bytes());
        frame.extend_from_slice(b"application/x-sh");
        assert!(decode(&frame).is_err());
    }
}
