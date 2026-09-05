//! Platform clipboard adapter for Clift.
//!
//! Reads the system clipboard once per invocation and exposes it to the core as
//! a plain snapshot. Platform bindings stay confined to this crate; no
//! Objective-C or Windows type may cross its public API.
//!
//! One platform, one module: `macos` and `windows` are siblings, and a third
//! platform is a third sibling rather than an edit to either. The composition
//! root sees one name, [`SystemClipboard`], and one flag, [`IS_SUPPORTED`].

// Reaching NSPasteboard requires Objective-C calls and the Win32 clipboard is
// a C interface, so this crate uses `unsafe`. Every `unsafe` block must carry
// an English comment stating the preconditions that make it sound.
#![deny(unsafe_op_in_unsafe_fn)]

mod drop_files;
mod offer;

#[cfg(target_os = "macos")]
mod macos;

#[cfg(target_os = "macos")]
pub use macos::{MacClipboard, write_offer, write_text};

/// The clipboard reader for the platform this binary was built for.
#[cfg(target_os = "macos")]
pub type SystemClipboard = MacClipboard;

#[cfg(windows)]
mod windows;

#[cfg(windows)]
pub use windows::{WindowsClipboard, write_offer, write_text};

/// The clipboard reader for the platform this binary was built for.
#[cfg(windows)]
pub type SystemClipboard = WindowsClipboard;

#[cfg(not(any(target_os = "macos", windows)))]
mod unsupported;

#[cfg(not(any(target_os = "macos", windows)))]
pub use unsupported::{MacClipboard, write_offer, write_text};

/// The clipboard reader for the platform this binary was built for: here,
/// one that refuses and says so.
#[cfg(not(any(target_os = "macos", windows)))]
pub type SystemClipboard = MacClipboard;

pub use offer::{Offer, Written};

/// Whether this build can read the clipboard at all.
///
/// `doctor` uses it to say "not built into this binary" instead of reporting
/// a refusal as a fault in the user's installation.
pub const IS_SUPPORTED: bool = cfg!(any(target_os = "macos", windows));
