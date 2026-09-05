//! macOS clipboard support.
//!
//! Every Objective-C call in Clift is inside this directory, behind
//! `#[cfg(target_os = "macos")]`. Support for another platform is a sibling
//! module, not an edit to this one.

mod files;
mod image;
mod pasteboard;
mod write;

pub use pasteboard::MacClipboard;
pub use write::{write_offer, write_text};
