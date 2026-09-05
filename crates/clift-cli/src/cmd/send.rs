//! `clift send [files...]`.
//!
//! The order here is the product. Nothing about it is arbitrary:
//!
//! 1. **work out what is being sent**: a plain text clipboard stops here, and
//!    stops before a single packet, which is what makes an ordinary paste free;
//! 2. **work out where it is going**: before connecting, so a mistake cannot
//!    connect to the wrong host;
//! 3. **send it**: limits, batch directory, uploads, all or nothing;
//! 4. **only then** produce the text to paste, and only then, if asked,
//!    replace the clipboard with it.

use crate::dto;
use crate::output::Reporter;
use crate::progress::{Narrating, Spinner};
use crate::system::{SystemClock, SystemIdSource};
use clift_core::config::{self, Format};
use clift_core::error::{CliftError, ErrorKind, Remedy, Stage};
use clift_core::ports::{ClipboardSource, TransportTarget};
use clift_core::usecase::{self, SendOutcome};
use std::path::PathBuf;

/// # Errors
/// Returns exit code 10 when there is nothing to send, which the terminal
/// integration turns into an ordinary paste. Otherwise propagates whatever
/// failed, and produces no text on any failure path.
pub fn run(
    files: &[String],
    from_clipboard: bool,
    to: Option<&str>,
    copy: bool,
    format: Option<&str>,
    reporter: &Reporter,
) -> Result<(), CliftError> {
    if let Some(name) = format {
        // The specification's other profiles are backlog. Accepting a name Clift cannot
        // honour would be a lie told in the one place the user is watching.
        if name != Format::Instruction.as_str() {
            return Err(CliftError::new(
                Stage::Config,
                ErrorKind::Config,
                format!("this build has no {name:?} format"),
            )
            .with_remedy(Remedy::new(
                "Use the only profile it has:",
                "clift send --format instruction <file>",
            )));
        }
    }

    // The reader owns the temporary file it writes for a clipboard image, so it
    // has to stay alive until the upload has finished with it.
    let clipboard = clift_clipboard::SystemClipboard::new();
    let source: Option<&dyn ClipboardSource> = if from_clipboard || files.is_empty() {
        Some(&clipboard)
    } else {
        None
    };

    let paths: Vec<PathBuf> = files.iter().map(PathBuf::from).collect();
    let resolved = usecase::resolve(&paths, source).map_err(nothing_to_send)?;

    let path = config::io::default_config_path()?;
    let loaded = config::io::load(&path)?;
    for warning in &loaded.warnings {
        reporter.warn(warning);
    }
    let (name, target) = usecase::resolve_send_target(&loaded.config, to)?;

    reporter.verbose(&format!(
        "sending {} attachment(s) to {name}",
        resolved.attachments().len()
    ));

    let transport = crate::system::transport(&loaded.config, reporter);
    // A send is nine or ten SSH operations. They share one connection when
    // reuse is available and open their own when it is not, and the
    // difference between those two is the difference between a wait and a
    // wait long enough to look like a hang.
    let spinner = Spinner::new(reporter.interactive());
    let narrating = Narrating::new(&transport, &spinner);
    let outcome = usecase::perform(
        &narrating,
        &TransportTarget::new(target.ssh_host()),
        resolved.attachments(),
        &usecase::SendPolicy {
            limits: loaded.config.defaults().limits(),
            remote_dir: Some(target.remote_dir()),
            retention: Some(loaded.config.defaults().retention()),
        },
        &SystemClock,
        &SystemIdSource,
    )?;
    // Before anything is rendered: the spinner owns the last line of stderr
    // until it is gone.
    drop(spinner);

    if let Some(warning) = outcome.inbox_warning() {
        reporter.warn(warning);
    }
    if let Some(note) = outcome.sweep_note() {
        // The occasional tidy-up, which never changes the outcome of the send.
        reporter.verbose(note);
    }

    render(&outcome, name.as_str(), copy, reporter)
}

/// Turns "nothing to send" into a message that says what to do about it.
fn nothing_to_send(error: CliftError) -> CliftError {
    if error.kind() != ErrorKind::NoAttachment || error.remedy().is_some() {
        return error;
    }
    error.with_remedy(Remedy::new(
        "Copy a file or an image first, or name the files:",
        "clift send <file>...",
    ))
}

fn render(
    outcome: &SendOutcome,
    target: &str,
    copy: bool,
    reporter: &Reporter,
) -> Result<(), CliftError> {
    // The clipboard is replaced before anything is printed, so that a failure
    // to replace it cannot happen after the user has been told it worked.
    if copy {
        clift_clipboard::write_text(outcome.insertion_text())?;
    }

    if reporter.json() {
        let document = dto::send(
            target,
            outcome.batch(),
            outcome.insertion_text().to_string(),
        );
        let value = serde_json::to_value(&document).map_err(|error| {
            CliftError::new(
                Stage::Internal,
                ErrorKind::Internal,
                "could not build the send document",
            )
            .with_source(error)
        })?;
        reporter.machine(&value).map_err(stdout_failed)?;
    } else {
        // The only thing on stdout: what the user pastes.
        reporter
            .insertion_text(&format!("{}\n", outcome.insertion_text()))
            .map_err(stdout_failed)?;
    }

    let count = outcome.batch().files().len();
    let noun = if count == 1 { "file" } else { "files" };
    reporter.success(&format!("Sent {count} {noun} to {target}."));
    if copy {
        // The specification requires saying so: the user's clipboard is not Clift's to
        // change quietly.
        reporter.success("The clipboard now holds the text above.");
    }
    Ok(())
}

fn stdout_failed(error: std::io::Error) -> CliftError {
    CliftError::new(
        Stage::Internal,
        ErrorKind::Internal,
        "could not write the result to stdout",
    )
    .with_source(error)
}
