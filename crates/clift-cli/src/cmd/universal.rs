//! `clift paste --mode universal`: seal, publish, and get the token in front of
//! the user.
//!
//! The composition root for Universal Mode. It resolves the relay, builds the
//! bundle, calls the use case, and then does the one thing the use case cannot:
//! decides how the token reaches the terminal.
//!
//! Delivery is the part with the interesting failure. Publishing succeeded, so
//! there is now a sealed object on somebody else's machine; if the token then
//! fails to reach the user, that object is a secret with no purpose, sitting
//! there until its TTL runs out. So a delivery failure withdraws it before
//! reporting anything. The TTL would get there eventually -- this makes it
//! seconds instead of minutes, and costs one request.

use crate::dto;
use crate::output::Reporter;
use crate::progress::Spinner;
use crate::relay;
use crate::system::SystemRandomness;
use clift_core::config::Config;
use clift_core::domain::LocalAttachment;
use clift_core::error::{CliftError, ErrorKind, Remedy, Stage};
use clift_core::format;
use clift_core::ports::Relay;
use clift_core::universal::{BundleEntry, RelaySettings, media};
use clift_core::usecase::{self, Published};
use clift_relay::HttpRelay;

/// How the token gets to the terminal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Delivery {
    /// Print it on stdout and let the caller decide. The default, and what a
    /// terminal adapter or a shell pipeline wants.
    Print,
    /// Put it on the clipboard for the user to paste themselves.
    Copy,
    /// Type it into whichever window has focus, leaving the clipboard alone.
    Inject,
    /// Print the bare token, with no instruction wrapped around it.
    ///
    /// What `clift copy` produces, on the machine the attachment is coming
    /// *from*. Bare because of what happens next: the user selects the line and
    /// the key press on their own machine has to tell it apart from the
    /// instruction `--copy` leaves on a clipboard. Those two must never be
    /// confused, or pressing the key twice after a `--copy` would redeem the
    /// object the user had just published.
    Token,
}

/// Runs the whole Universal Mode paste.
///
/// # Errors
/// Fails when no relay is configured, when the attachment is not something
/// Universal Mode carries, when the relay refuses or cannot be reached, and
/// when the chosen delivery cannot be performed.
pub fn run(
    config: &Config,
    attachments: &[LocalAttachment],
    delivery: Delivery,
    oversize: &Remedy,
    reporter: &Reporter,
) -> Result<(), CliftError> {
    let settings = relay::settings(config)?;
    let entries = bundle_entries(attachments, settings.max_object_bytes(), oversize)?;
    let client = HttpRelay::new(&settings);

    reporter.verbose(&format!(
        "sealing {} attachment(s) for {}",
        entries.len(),
        settings.url()
    ));

    // Encrypting a few megabytes is instant; the upload is not, and a user who
    // pressed a key deserves to see that something is happening.
    let spinner = Spinner::new(reporter.interactive());
    spinner.begin("uploading the sealed attachment".to_string());
    let published = usecase::publish(&entries, &settings, &client, &SystemRandomness)?;
    drop(spinner);

    let insertion_text = match delivery {
        // The token by itself. `clift copy` is read off a terminal by a person
        // who then selects the line, and an instruction around it would be
        // selected too.
        Delivery::Token => published.token().expose(),
        _ => format::render_token(published.token(), entries.len()),
    };

    if let Err(error) = deliver(delivery, &insertion_text) {
        withdraw(&client, &published, reporter);
        return Err(error);
    }

    report(&published, &settings, &insertion_text, delivery, reporter)
}

/// Reads each attachment and gives it a media type Universal Mode will carry.
fn bundle_entries(
    attachments: &[LocalAttachment],
    ceiling: u64,
    oversize: &Remedy,
) -> Result<Vec<BundleEntry>, CliftError> {
    let mut entries = Vec::with_capacity(attachments.len());
    for attachment in attachments {
        // Checked before the read, so a large file is refused rather than
        // loaded into memory first.
        if attachment.size() > ceiling {
            return Err(too_large(attachment, ceiling, oversize));
        }
        let Some(media_type) = media::from_extension(attachment.name()) else {
            return Err(CliftError::new(
                Stage::Relay,
                ErrorKind::LimitExceeded,
                format!(
                    "Universal Mode does not carry {:?}; it carries {}",
                    attachment.name().as_str(),
                    media::carried_types().join(", ")
                ),
            )
            .with_remedy(oversize.clone()));
        };
        let data = std::fs::read(attachment.path()).map_err(|error| {
            CliftError::new(
                Stage::Clipboard,
                ErrorKind::ClipboardRead,
                format!("cannot read {}", attachment.path().display()),
            )
            .with_source(error)
        })?;
        entries.push(BundleEntry::new(
            attachment.name().clone(),
            media_type,
            data,
        )?);
    }
    Ok(entries)
}

fn too_large(attachment: &LocalAttachment, ceiling: u64, oversize: &Remedy) -> CliftError {
    CliftError::new(
        Stage::Relay,
        ErrorKind::LimitExceeded,
        format!(
            "{} is {} bytes, over the {ceiling} this relay accepts",
            attachment.name().as_str(),
            attachment.size()
        ),
    )
    .with_remedy(oversize.clone())
}

/// The way out when an attachment will not fit through the relay, on the
/// machine the user is pasting *from*.
#[must_use]
pub fn send_it_over_ssh_instead() -> Remedy {
    Remedy::new(
        "Send it over your own SSH connection instead:",
        "clift send --clipboard --to <target>",
    )
}

/// Puts the token where the user will find it.
///
/// Only `--copy` touches the clipboard. `--inject` types the token in, which
/// is why nothing here has to put a screenshot back afterwards: the user's
/// clipboard is never borrowed in the first place.
fn deliver(delivery: Delivery, text: &str) -> Result<(), CliftError> {
    match delivery {
        // Nothing to do yet: printing happens in `report`, on stdout, after
        // everything that could still fail has succeeded.
        Delivery::Print | Delivery::Token => Ok(()),
        Delivery::Copy => clift_clipboard::write_text(text),
        Delivery::Inject => clift_inject::type_into_focused_window(text),
    }
}

/// Takes back an object whose token never reached anybody.
fn withdraw(client: &HttpRelay, published: &Published, reporter: &Reporter) {
    match client.revoke(published.token().id()) {
        Ok(()) => reporter.verbose("the published object was withdrawn"),
        // Swallowed, because the caller is already failing for a better reason
        // and the object expires on its own. Said out loud so that a user
        // watching `--verbose` knows something is still on the relay.
        Err(error) => reporter.warn(&format!(
            "the attachment could not be withdrawn from the relay and will expire on its own: {error}"
        )),
    }
}

/// The last step, and the only one allowed to touch stdout.
fn report(
    published: &Published,
    settings: &RelaySettings,
    insertion_text: &str,
    delivery: Delivery,
    reporter: &Reporter,
) -> Result<(), CliftError> {
    if reporter.json() {
        let document = dto::universal_paste(published, settings.url(), insertion_text.to_string());
        let value = serde_json::to_value(&document).map_err(|error| {
            CliftError::new(
                Stage::Internal,
                ErrorKind::Internal,
                format!("could not build the paste document: {error}"),
            )
        })?;
        return reporter.machine(&value).map_err(stdout_failed);
    }

    match delivery {
        Delivery::Print | Delivery::Token => reporter
            .insertion_text(&format!("{insertion_text}\n"))
            .map_err(stdout_failed),
        Delivery::Copy => {
            reporter.success(&summary(
                published,
                "The clipboard now holds the instruction",
            ));
            Ok(())
        }
        Delivery::Inject => {
            reporter.success(&summary(
                published,
                "Typed into the focused window; your clipboard is untouched",
            ));
            Ok(())
        }
    }
}

fn summary(published: &Published, lead: &str) -> String {
    let count = published.entries().len();
    let noun = if count == 1 {
        "attachment"
    } else {
        "attachments"
    };
    format!(
        "{lead}. {count} {noun}, {} bytes sealed, valid for {} seconds.",
        published.sealed_bytes(),
        published.ttl().as_secs()
    )
}

fn stdout_failed(error: std::io::Error) -> CliftError {
    CliftError::new(
        Stage::Internal,
        ErrorKind::Internal,
        "could not write the result to stdout",
    )
    .with_source(error)
}
