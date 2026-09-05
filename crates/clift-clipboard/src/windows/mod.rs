//! Windows clipboard support.
//!
//! Every Win32 call Clift makes to the clipboard is inside this directory,
//! behind `#[cfg(windows)]`. It is a sibling of `macos`, not an edit to it, and
//! what leaves it is the same plain [`clift_core::ports::ClipboardSnapshot`].
//!
//! The shape is the same as on macOS with one difference worth knowing: a
//! Windows screenshot (`Win+Shift+S`, `PrtScn`) usually arrives as a device
//! independent bitmap rather than as PNG, so the image is re-encoded here with
//! GDI+, which every Windows installation has, rather than sent as several
//! megabytes of raw pixels. An application that also offers the registered
//! `PNG` format gets its bytes passed through untouched.
//!
//! Everything in this module has been compiled for Windows by the release
//! pipeline; whether it reads a real screenshot is for a real Windows machine
//! to say, and until one has, nothing here is described as verified.

mod files;
mod gdiplus;
mod image;
mod reader;
mod session;
mod write;

pub use reader::WindowsClipboard;
pub use write::{write_offer, write_text};
