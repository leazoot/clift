//! What one redeemed attachment is offered to the clipboard as.
//!
//! A clipboard holds several representations of the same thing at once and the
//! application doing the pasting picks the one it understands. Offering only
//! one of them is a decision about which applications get to work, and the
//! wrong one is invisible: an attachment that arrived, a clipboard deliberately
//! left alone, and a user who saw nothing happen.
//!
//! Which representations an attachment gets is a product rule and lives in
//! `clift_core::universal::media::clipboard_form`. This is only the shape the
//! two platform modules agree on.

use std::path::Path;

/// One attachment, in every form this build can offer it.
#[derive(Debug, Clone, Copy)]
pub struct Offer<'a> {
    /// The file a file manager should receive. For a batch of several
    /// attachments this is the directory holding them, so that one paste in
    /// Explorer or Finder produces all of them rather than an arbitrary one.
    pub file: &'a Path,
    /// PNG bytes, when the attachment is one whose signature checked out.
    pub image: Option<&'a [u8]>,
    /// What a cursor in a document should receive: the attachment's own text
    /// when it has any, and otherwise its absolute path.
    ///
    /// Never empty. A path is a poor thing to paste into prose, and it is still
    /// far better than a key press that appears to do nothing.
    pub text: &'a str,
}

/// Which representations actually reached the clipboard.
///
/// Reported rather than assumed, because the caller tells the user what
/// happened and "the clipboard now holds your image" has to be true. A
/// representation the clipboard refuses is not a failure of the whole paste:
/// three out of four applications working beats an error and an untouched
/// clipboard.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Written {
    pub file: bool,
    pub image: bool,
    pub text: bool,
}
