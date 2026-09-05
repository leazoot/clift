//! `clift clean [target]`.
//!
//! Three modes, in increasing order of how much they remove: expired batches
//! (the default), batches older than a stated age, and everything. Only the
//! last one asks first -- and, on a machine with nobody to ask, refuses rather
//! than waits.

use crate::output::Reporter;
use crate::progress::{Narrating, Spinner};
use clift_core::config::{self, Config};
use clift_core::context::{Confirmation, confirmation_for};
use clift_core::error::{CliftError, ErrorKind, Remedy, Stage};
use clift_core::ports::TransportTarget;
use clift_core::staging::{Action, CleanReport, Retention, clean, locate_inbox};
use clift_core::usecase;
use std::io::{BufRead, IsTerminal, Write};
use std::time::SystemTime;

/// # Errors
/// Propagates configuration and transport failures. A batch that cannot be
/// removed is reported rather than raised: one stuck directory must not stop
/// the rest of the inbox from being tidied.
pub fn run(
    target: Option<&str>,
    all: bool,
    older_than: Option<&str>,
    yes: bool,
    dry_run: bool,
    reporter: &Reporter,
) -> Result<(), CliftError> {
    let path = config::io::default_config_path()?;
    let loaded = config::io::load(&path)?;
    for warning in &loaded.warnings {
        reporter.warn(warning);
    }
    let (name, entry) = usecase::resolve_send_target(&loaded.config, target)?;

    let retention = retention_for(&loaded.config, all, older_than)?;

    if all && !dry_run {
        match confirmation_for(
            &format!("removing every batch on {name}"),
            stdin_is_a_terminal(),
            yes,
        )? {
            Confirmation::AlreadyGiven => {}
            Confirmation::Ask => {
                if !ask(&format!("Remove every batch from {name}?"))? {
                    return Err(cancelled(name.as_str()));
                }
            }
        }
    }

    let transport = crate::system::transport(&loaded.config, reporter);
    let spinner = Spinner::new(reporter.interactive());
    let narrating = Narrating::new(&transport, &spinner);
    let host = TransportTarget::new(entry.ssh_host());
    // Located, not ensured: cleaning is not a reason to create an inbox that is
    // not there.
    let inbox = locate_inbox(&narrating, &host, Some(entry.remote_dir()))?;
    if let Some(warning) = inbox.warning() {
        reporter.warn(&warning);
    }

    let action = if dry_run {
        Action::Report
    } else {
        Action::Remove
    };
    let report = clean(
        &narrating,
        &host,
        inbox.root(),
        retention,
        action,
        SystemTime::now(),
    )?;

    drop(spinner);
    render(&report, name.as_str(), dry_run, reporter)
}

/// Which batches this run is about.
fn retention_for(
    config: &Config,
    all: bool,
    older_than: Option<&str>,
) -> Result<Retention, CliftError> {
    if all {
        if older_than.is_some() {
            return Err(CliftError::new(
                Stage::Config,
                ErrorKind::Config,
                "--all and --older-than ask for different things",
            )
            .with_remedy(Remedy::new("Pick one:", "clift clean --older-than 7d")));
        }
        return Ok(Retention::Everything);
    }
    match older_than {
        Some(value) => config::units::parse_duration(value)
            .map(Retention::OlderThan)
            .map_err(|error| error.into_clift(Stage::Config, ErrorKind::Config)),
        None => Ok(Retention::OlderThan(config.defaults().retention())),
    }
}

fn render(
    report: &CleanReport,
    target: &str,
    dry_run: bool,
    reporter: &Reporter,
) -> Result<(), CliftError> {
    if reporter.json() {
        return reporter
            .machine(&serde_json::json!({
                "schema_version": 1,
                "status": "ok",
                "target": target,
                "dry_run": dry_run,
                "batches": report.batches,
                "files": report.files,
                "bytes": report.bytes,
                "skipped": report.skipped,
            }))
            .map_err(stdout_failed);
    }

    let verb = if dry_run { "Would remove" } else { "Removed" };
    if report.batches == 0 {
        reporter.success(&format!("Nothing to remove on {target}."));
    } else {
        reporter.success(&format!(
            "{verb} {} batch(es), {} file(s), {} on {target}.",
            report.batches,
            report.files,
            human_size(report.bytes)
        ));
    }
    for skipped in &report.skipped {
        // Not a failure, and not silent either: each of these is something the
        // user may want to look at by hand.
        reporter.warn(&format!("left alone -- {skipped}"));
    }
    Ok(())
}

fn human_size(bytes: u64) -> String {
    const UNITS: [(u64, &str); 3] = [
        (1024 * 1024 * 1024, "GiB"),
        (1024 * 1024, "MiB"),
        (1024, "KiB"),
    ];
    for (scale, name) in UNITS {
        if bytes >= scale {
            return format!("{:.1} {name}", bytes as f64 / scale as f64);
        }
    }
    format!("{bytes} bytes")
}

fn stdin_is_a_terminal() -> bool {
    std::io::stdin().is_terminal()
}

fn ask(question: &str) -> Result<bool, CliftError> {
    let mut stderr = std::io::stderr();
    let _ = write!(stderr, "\n{question} [y/N] ");
    let _ = stderr.flush();

    let mut answer = String::new();
    std::io::stdin()
        .lock()
        .read_line(&mut answer)
        .map_err(|error| {
            CliftError::new(
                Stage::Config,
                ErrorKind::Config,
                "could not read the answer from the terminal",
            )
            .with_source(error)
        })?;
    let answer = answer.trim().to_ascii_lowercase();
    Ok(answer == "y" || answer == "yes")
}

fn cancelled(target: &str) -> CliftError {
    CliftError::new(
        Stage::Config,
        ErrorKind::Config,
        format!("cleaning {target} was cancelled"),
    )
    .with_remedy(Remedy::new(
        "Remove only the expired batches instead:",
        format!("clift clean {target}"),
    ))
}

fn stdout_failed(error: std::io::Error) -> CliftError {
    CliftError::new(
        Stage::Internal,
        ErrorKind::Internal,
        "could not write the result to stdout",
    )
    .with_source(error)
}
