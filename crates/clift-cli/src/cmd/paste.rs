//! `clift paste`.
//!
//! What a terminal calls instead of pasting. The difference from `send` is not
//! in what it does but in what it promises: the exit code is the whole
//! interface. `0` means "here is text to type", `10` means
//! "nothing to send, do your own paste", and anything else means "say something
//! and touch nothing".
//!
//! The `10` path is the one that has to be fast and free, because it is the one
//! that happens every time somebody pastes ordinary text. It reads the
//! clipboard and returns; no configuration is loaded, no host is resolved, no
//! relay is contacted and no connection is opened. Universal Mode changed a
//! great deal about this command and it did not change that.
//!
//! Which mode runs is decided in [`chosen_mode`].
//!
//! The clipboard arrives as an argument rather than being opened here, because
//! the hotkey helper has already read it: one press has to look at the
//! clipboard exactly once, and it has to look before it can know whether this
//! press is a paste at all.

use crate::cmd::universal::{self, Delivery};
use crate::dto;
use crate::output::Reporter;
use crate::progress::{Narrating, Spinner};
use crate::relay;
use crate::system::{SystemClock, SystemIdSource};
use clift_core::config::{self, Config, Mode};
use clift_core::error::{CliftError, ErrorKind, Remedy, Stage};
use clift_core::ports::{ClipboardSource, TransportTarget};
use clift_core::usecase::{self, Resolved, SendPolicy};

/// # Errors
/// Returns exit code 10 when the clipboard holds nothing to upload, which the
/// terminal turns into an ordinary paste. Every other failure leaves stdout
/// empty, because whatever is on stdout gets typed into somebody's prompt.
pub fn run(
    mode: Option<&str>,
    to: Option<&str>,
    copy: bool,
    inject: bool,
    source: &dyn ClipboardSource,
    reporter: &Reporter,
) -> Result<(), CliftError> {
    // Argument validation first, and before the clipboard is touched. A
    // misspelled `--mode` is wrong whatever is on the clipboard, and answering
    // "nothing to send" to it would send the user looking in the wrong place.
    // This is pure parsing, so it costs the ordinary-paste path nothing.
    let requested = parse_mode(mode)?;
    if requested == Some(Mode::Universal) {
        refuse_a_target_in_universal_mode(to)?;
    }

    // First, and before anything else exists: an ordinary paste must cost
    // nothing at all.
    let resolved = match usecase::resolve(&[], Some(source)) {
        Ok(resolved) => resolved,
        Err(error) if error.kind() == ErrorKind::NoAttachment => {
            // Still exit code 10; the document is so that an adapter can tell
            // this apart from a failure without reading prose.
            if reporter.json() {
                let value = serde_json::to_value(dto::no_attachment())
                    .map_err(|error| serialisation_failed(&error))?;
                reporter.machine(&value).map_err(stdout_failed)?;
            }
            return Err(error);
        }
        Err(error) => return Err(error),
    };

    let path = config::io::default_config_path()?;
    let loaded = config::io::load(&path)?;
    for warning in &loaded.warnings {
        reporter.warn(warning);
    }

    // `relay::is_configured` rather than `config.relay()`: CLIFT_RELAY_URL is a
    // way of configuring a relay, and a mode resolution that ignored it would
    // send a user who set only the variable down the Fast Mode path, to fail
    // for a reason that has nothing to do with what they asked for.
    let mode = chosen_mode(
        requested,
        &loaded.config,
        relay::is_configured(&loaded.config),
    );
    reporter.verbose(&format!("{} mode", mode.as_str()));

    match mode {
        Mode::Universal => {
            // Checked again, because the mode may have come from the
            // configuration rather than the flag and the answer is the same
            // either way.
            refuse_a_target_in_universal_mode(to)?;
            universal::run(
                &loaded.config,
                resolved.attachments(),
                delivery(copy, inject),
                &universal::send_it_over_ssh_instead(),
                reporter,
            )
        }
        Mode::Fast => fast(&loaded.config, &resolved, to, copy, reporter),
    }
}

/// Reads the `--mode` argument. Pure, and therefore free.
///
/// # Errors
/// Refuses anything that is not one of the two modes. No fuzzy matching: the specification
/// requires an unrecognised argument to fail rather than be guessed at, and a
/// guess here chooses how somebody's screenshot travels.
fn parse_mode(requested: Option<&str>) -> Result<Option<Mode>, CliftError> {
    match requested {
        None => Ok(None),
        Some("universal") => Ok(Some(Mode::Universal)),
        Some("fast") => Ok(Some(Mode::Fast)),
        Some(other) => Err(CliftError::new(
            Stage::Config,
            ErrorKind::Config,
            format!("unknown mode {other:?}"),
        )
        .with_remedy(Remedy::new(
            "The two modes are:",
            "clift paste --mode universal | --mode fast",
        ))),
    }
}

/// `--to` names a host, and Universal Mode does not send to one.
///
/// Refused rather than ignored. Silently dropping the flag would leave the user
/// believing something untrue about where their attachment went, which is the
/// failure the specification's second red line exists to prevent -- even though nothing
/// here would actually reach the wrong host.
fn refuse_a_target_in_universal_mode(to: Option<&str>) -> Result<(), CliftError> {
    let Some(target) = to else {
        return Ok(());
    };
    Err(CliftError::new(
        Stage::TargetResolution,
        ErrorKind::Config,
        format!(
            "--to {target:?} names a host, and Universal Mode does not send to one: the host \
             that redeems the token is whichever one you paste it into"
        ),
    )
    .with_remedy(Remedy::new(
        "Send to that host over SSH instead:",
        format!("clift paste --mode fast --to {target}"),
    )))
}

/// Which mode this invocation runs in, once the flag has been parsed.
///
/// 1. `--mode`, if given.
/// 2. Whatever the configuration resolves to, which is Universal when a relay
///    is configured and Fast when one is not.
fn chosen_mode(requested: Option<Mode>, config: &Config, relay_configured: bool) -> Mode {
    match requested {
        Some(mode) => mode,
        None => config.mode_with_relay(relay_configured),
    }
}

const fn delivery(copy: bool, inject: bool) -> Delivery {
    if inject {
        Delivery::Inject
    } else if copy {
        Delivery::Copy
    } else {
        Delivery::Print
    }
}

/// Fast Mode: exactly what `clift paste` did before v2.0.
fn fast(
    config: &Config,
    resolved: &Resolved,
    to: Option<&str>,
    copy: bool,
    reporter: &Reporter,
) -> Result<(), CliftError> {
    let (name, target) = usecase::resolve_send_target(config, to)?;

    let transport = crate::system::transport(config, reporter);
    // Nine or ten SSH operations, sharing one connection where reuse is
    // available. Long enough either way to need saying.
    let spinner = Spinner::new(reporter.interactive());
    let narrating = Narrating::new(&transport, &spinner);
    let outcome = usecase::perform(
        &narrating,
        &TransportTarget::new(target.ssh_host()),
        resolved.attachments(),
        &SendPolicy {
            limits: config.defaults().limits(),
            remote_dir: Some(target.remote_dir()),
            retention: Some(config.defaults().retention()),
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
        reporter.verbose(note);
    }

    if reporter.json() {
        let document = dto::send(
            name.as_str(),
            outcome.batch(),
            outcome.insertion_text().to_string(),
        );
        let value =
            serde_json::to_value(&document).map_err(|error| serialisation_failed(&error))?;
        return reporter.machine(&value).map_err(stdout_failed);
    }

    if copy {
        // Only now, after the upload succeeded: replacing the clipboard and
        // then failing would cost the user twice.
        clift_clipboard::write_text(outcome.insertion_text())?;
        reporter.success(&format!(
            "Sent to {name}. The clipboard now holds the text to paste."
        ));
        return Ok(());
    }

    // Without `--json` the insertion text is still the result, so a person can
    // run `clift paste` by hand and see what the terminal would have typed.
    reporter
        .insertion_text(&format!("{}\n", outcome.insertion_text()))
        .map_err(stdout_failed)
}

fn serialisation_failed(error: &serde_json::Error) -> CliftError {
    CliftError::new(
        Stage::Internal,
        ErrorKind::Internal,
        format!("could not build the paste document: {error}"),
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

#[cfg(test)]
mod tests {
    use super::*;

    fn config(source: &str) -> Config {
        config::parse(source)
            .unwrap_or_else(|error| panic!("{error}"))
            .config
    }

    const WITH_RELAY: &str = "version = 1\n[relay]\nurl = \"https://relay.example.com\"\n";

    #[test]
    fn an_explicit_mode_always_wins() {
        for (flag, expected) in [("fast", Mode::Fast), ("universal", Mode::Universal)] {
            let requested = parse_mode(Some(flag)).unwrap_or_else(|error| panic!("{error}"));
            assert_eq!(chosen_mode(requested, &config(WITH_RELAY), true), expected);
        }
    }

    #[test]
    fn without_a_relay_the_answer_is_always_fast() {
        assert_eq!(
            chosen_mode(None, &config("version = 1\n"), false),
            Mode::Fast
        );
    }

    #[test]
    fn a_configured_relay_makes_universal_the_default() {
        assert_eq!(
            chosen_mode(None, &config(WITH_RELAY), true),
            Mode::Universal
        );

        // And a relay that came only from the environment counts just the
        // same. Without this, setting CLIFT_RELAY_URL and nothing else sends
        // the user down the Fast Mode path, to fail over a missing target they
        // never asked about.
        assert_eq!(
            chosen_mode(None, &config("version = 1\n"), true),
            Mode::Universal
        );
    }

    #[test]
    fn the_file_can_pin_the_mode_against_the_presence_of_a_relay() {
        let pinned = config("version = 1\nmode = \"fast\"\n[relay]\nurl = \"https://r.example\"\n");
        assert_eq!(chosen_mode(None, &pinned, true), Mode::Fast);
    }

    #[test]
    fn an_unknown_mode_is_refused_rather_than_guessed_at() {
        for wrong in ["univarsal", "Fast", "", "universal ", "auto"] {
            let error = parse_mode(Some(wrong)).unwrap_err();
            assert_eq!(error.kind(), ErrorKind::Config, "{wrong:?}");
            assert!(error.remedy().is_some(), "{wrong:?}");
        }
        assert_eq!(
            parse_mode(None).unwrap_or_else(|error| panic!("{error}")),
            None
        );
    }

    #[test]
    fn a_target_is_refused_in_universal_mode_and_accepted_in_its_absence() {
        assert!(refuse_a_target_in_universal_mode(None).is_ok());
        let error = refuse_a_target_in_universal_mode(Some("core")).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Config);
        assert!(
            error
                .remedy()
                .is_some_and(|remedy| remedy.command().contains("--mode fast"))
        );
    }

    #[test]
    fn the_delivery_flags_map_onto_the_three_ways_out() {
        assert_eq!(delivery(false, false), Delivery::Print);
        assert_eq!(delivery(true, false), Delivery::Copy);
        assert_eq!(delivery(false, true), Delivery::Inject);
        // clap refuses both together; if it ever stopped, injecting is the more
        // specific request and the one that also copies.
        assert_eq!(delivery(true, true), Delivery::Inject);
    }
}
