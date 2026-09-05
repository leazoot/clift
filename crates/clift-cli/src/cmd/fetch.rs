//! `clift fetch '<token>'`.
//!
//! The command an agent runs. It is the only Clift command whose normal caller
//! is a program rather than a person, and everything about its output follows
//! from that:
//!
//! - **stdout carries the paths and nothing else.** One absolute path per
//!   line, so `$(clift fetch ...)` is a usable thing to write. Every note,
//!   warning and error goes to stderr, because an agent that reads a warning
//!   as a path will go looking for a file called "warning". `--print-path`
//!   asked for this in earlier versions and is still accepted, because lines
//!   pasted by those versions are still out there.
//! - **nothing is printed unless the attachment is on disk.** A path an agent
//!   cannot open is worse than no path: it turns "the fetch failed" into "the
//!   file is corrupt", and sends the user looking in the wrong place.
//! - **the token is never echoed.** Not in a log line, not in an error, not
//!   under `--debug`. It goes into the shell's history as it is, and Clift is
//!   not going to add a second copy in a place the user is less likely to look.
//!
//! `--copy` is the one caller that is a person rather than a program: the
//! return trip, where a token published by `clift copy` on a server is redeemed
//! here and the image goes onto this machine's clipboard. It keeps stdout empty
//! for the same reason `paste --copy` does -- the result went somewhere else,
//! and printing it as well would put an attachment's path into a prompt nobody
//! asked to have it in.

use crate::dto;
use crate::output::Reporter;
use crate::relay;
use crate::system::{SystemClock, SystemIdSource};
use clift_clipboard::{Offer, Written};
use clift_core::config;
use clift_core::domain::LocalPath;
use clift_core::error::{CliftError, ErrorKind, Remedy, Stage};
use clift_core::staging::{self, WrittenBatch};
use clift_core::universal::media::ClipboardForm;
use clift_core::universal::{Token, media};
use clift_core::usecase;
use clift_relay::HttpRelay;
use std::path::Path;

/// # Errors
/// Returns 27 for a token that cannot be redeemed, 28 when the relay cannot be
/// reached, 29 when what arrives does not authenticate, 25 when the attachment
/// cannot be written, and 24 when `--copy` was asked for and the clipboard
/// refused it.
pub fn run(
    token: &str,
    print_path: bool,
    copy: bool,
    reporter: &Reporter,
) -> Result<(), CliftError> {
    // Parsed before anything else, so a mangled paste costs no network at all
    // and says exactly what is wrong with it.
    redeem(&Token::parse(token)?, print_path, copy, reporter)
}

/// As [`run`], for a caller that has already parsed the token.
///
/// The hotkey helper is that caller: it has to parse the clipboard to know
/// whether this press is a redemption at all, and turning the result back into
/// a string so this function could parse it again would put the key material
/// into one more `String` for no gain, and add a fourth caller to
/// `Token::expose` that had nothing to expose it for.
///
/// # Errors
/// As [`run`].
pub fn redeem(
    token: &Token,
    print_path: bool,
    copy: bool,
    reporter: &Reporter,
) -> Result<(), CliftError> {
    reporter.verbose(&format!("redeeming {}", token.redacted()));

    let path = config::io::default_config_path()?;
    let loaded = config::io::load(&path)?;
    for warning in &loaded.warnings {
        reporter.warn(warning);
    }

    let settings = relay::settings_for_fetch(&loaded.config)?;
    let client = HttpRelay::new(&settings);
    let inbox = staging::local_inbox_root()?;

    let fetched =
        usecase::fetch(token, &inbox, &client, &SystemClock, &SystemIdSource).map_err(|error| {
            if copy {
                spent_the_other_way(error)
            } else {
                error
            }
        })?;
    let paths = fetched.batch().paths();

    if reporter.json() {
        let document = dto::fetch(fetched.batch());
        let value = serde_json::to_value(&document).map_err(|error| {
            CliftError::new(
                Stage::Internal,
                ErrorKind::Internal,
                format!("could not build the fetch document: {error}"),
            )
        })?;
        return reporter.machine(&value).map_err(stdout_failed);
    }

    if print_path {
        reporter.verbose("--print-path is the default now and can be left out");
    }
    let count = paths.len();
    let noun = if count == 1 {
        "attachment"
    } else {
        "attachments"
    };
    reporter.verbose(&format!(
        "fetched {count} {noun} into {}",
        fetched.batch().directory()
    ));

    if copy {
        return onto_clipboard(fetched.batch(), reporter);
    }

    let rendered = paths
        .iter()
        .map(|path| format!("{path}\n"))
        .collect::<String>();
    reporter.insertion_text(&rendered).map_err(stdout_failed)
}

/// Points a spent token at the machine it came from, not at this one.
///
/// The relay client raises this failure and cannot know which direction the
/// token was travelling, so it offers the outward remedy: publish again from
/// here. On a `--copy` run that is advice nobody should follow -- the file is on
/// the other machine, and running `clift paste` here would send whatever
/// happens to be on this clipboard to somebody else. Only the remedy changes;
/// the stage, the kind and the cause chain stay the relay's own.
fn spent_the_other_way(error: CliftError) -> CliftError {
    if error.kind() != ErrorKind::TokenUnusable {
        return error;
    }
    error.with_remedy(Remedy::new(
        "Tokens are single use. Make another one where the file is:",
        "clift copy <file>",
    ))
}

/// `--copy`: offer the attachment to this machine's clipboard.
///
/// Everything landed on disk first, whatever happens here, and the paths are
/// always said -- on stderr, because `--copy` promised stdout would stay empty.
///
/// The clipboard then gets **every form of it this build can produce**: the
/// file, so a paste in a folder makes a copy of it; the pixels, when it is a
/// picture; and text, which is the attachment's own content when it has any and
/// its path when it has not. The last of those is the one that matters most and
/// was missing: a markdown file used to land silently, and a person who pressed
/// a key saw nothing at all happen.
fn onto_clipboard(batch: &WrittenBatch, reporter: &Reporter) -> Result<(), CliftError> {
    for file in batch.files() {
        reporter.success(&format!("  {}", file.path()));
    }

    // What a paste in a folder should produce: the attachment when there is
    // one, the directory holding them when there are several. A clipboard can
    // hold one image and one piece of text, but "the folder with all of it in"
    // is a sensible answer for any number.
    let (subject, form, contents) = match batch.files() {
        [only] => {
            let bytes = read_back(only.path())?;
            let form = media::clipboard_form(only.media_type(), &bytes);
            (only.path().as_str(), form, Some(bytes))
        }
        _ => (batch.directory().as_str(), ClipboardForm::FileOnly, None),
    };

    let text = match (form, &contents) {
        // Lossless: `clipboard_form` only answers `Text` after checking that
        // the bytes are valid UTF-8.
        (ClipboardForm::Text, Some(bytes)) => String::from_utf8_lossy(bytes).into_owned(),
        _ => subject.to_string(),
    };
    let image = match (form, &contents) {
        (ClipboardForm::Image, Some(bytes)) => Some(bytes.as_slice()),
        _ => None,
    };

    let written = clift_clipboard::write_offer(&Offer {
        file: Path::new(subject),
        image,
        text: &text,
    })?;
    reporter.success(&describe(&written, form, batch.files().len()));
    Ok(())
}

/// Says what the clipboard actually took, not what it was offered.
///
/// The difference is the whole point of reporting it: a user who is told their
/// picture is on the clipboard will go and paste it, and if only the path made
/// it they need to know before they wonder why their image editor is empty.
fn describe(written: &Written, form: ClipboardForm, count: usize) -> String {
    let mut parts = Vec::new();
    if written.image {
        parts.push("the image");
    }
    if written.text {
        parts.push(match form {
            ClipboardForm::Text => "its text",
            _ => "its path",
        });
    }
    if written.file {
        parts.push(if count > 1 {
            "the folder holding them"
        } else {
            "the file itself"
        });
    }

    let held = match parts.as_slice() {
        [] => return "The clipboard took none of it and is unchanged.".to_string(),
        [one] => (*one).to_string(),
        [first, rest @ ..] => format!("{first} and {}", rest.join(" and ")),
    };
    format!("The clipboard now holds {held}. Paste it anywhere.")
}

/// Reads back what was just written, to hand the bytes to the clipboard.
fn read_back(path: &LocalPath) -> Result<Vec<u8>, CliftError> {
    std::fs::read(path.as_str()).map_err(|error| {
        CliftError::new(
            Stage::Staging,
            ErrorKind::RemoteDirectory,
            format!("cannot read back {path}"),
        )
        .with_source(error)
    })
}

fn stdout_failed(error: std::io::Error) -> CliftError {
    CliftError::new(
        Stage::Internal,
        ErrorKind::Internal,
        "could not write the result to stdout",
    )
    .with_source(error)
}
