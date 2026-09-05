//! `clift setup <ssh-host>`, and `clift setup` on its own.
//!
//! With a host, the composition root for the Fast Mode setup flow: it builds
//! the transport, asks the user to confirm what `ssh` says the host is, runs
//! the checks in `clift-core`, and only then writes the configuration. With no
//! host, the first-time conversation in `first_run`.
//!
//! The confirmation is deliberately before anything is dialled. The specification wants
//! the user to see the resolved user, host and port and agree to them, and a
//! question asked after the connection succeeded would be a question about
//! something that has already happened.

use crate::output::Reporter;
use crate::progress::{Narrating, Spinner};
use crate::prompt::Console;
use crate::system::SystemClock;
use clift_core::config;
use clift_core::context::{Confirmation, confirmation_for};
use clift_core::domain::TargetName;
use clift_core::error::{CliftError, ErrorKind, Remedy, Stage};
use clift_core::usecase::{SetupReport, prepare_target};
use std::io::IsTerminal;

/// # Errors
/// Propagates every failure of whichever flow ran.
pub fn run(
    ssh_host: Option<&str>,
    assume_yes: bool,
    reporter: &Reporter,
) -> Result<(), CliftError> {
    match ssh_host {
        Some(ssh_host) => configure_host(ssh_host, assume_yes, reporter),
        None => super::first_run::run(reporter),
    }
}

/// The Fast Mode flow for one host.
///
/// # Errors
/// Propagates every failure of the flow. Nothing is written to the
/// configuration unless all four checks passed.
pub fn configure_host(
    ssh_host: &str,
    assume_yes: bool,
    reporter: &Reporter,
) -> Result<(), CliftError> {
    let name = TargetName::new(ssh_host)
        .map_err(|error| error.into_clift(Stage::Config, ErrorKind::Config))?;

    // Read before anything is asked or connected: a configuration Clift cannot
    // parse is not a reason to prompt the user first and fail afterwards, and
    // the connection settings below come out of it.
    let path = config::io::default_config_path()?;
    let loaded = config::io::load(&path)?;
    for warning in &loaded.warnings {
        reporter.warn(warning);
    }

    let transport = crate::system::transport(&loaded.config, reporter);
    let settings = transport.settings_for(ssh_host)?;

    // What the user is being asked to agree to, before a single packet.
    reporter.success(&format!("Target:  {ssh_host}"));
    reporter.success(&format!("Remote:  {}", settings.summary()));

    match confirmation_for(
        &format!("setting up {ssh_host}"),
        std::io::stdin().is_terminal(),
        assume_yes,
    )? {
        Confirmation::AlreadyGiven => {}
        Confirmation::Ask => {
            if !ask(&format!("Set up {ssh_host}?"))? {
                return Err(cancelled(ssh_host));
            }
        }
    }

    // Each of the four checks is one or two SSH sessions, and on a distant host
    // that is three to five seconds each. The spinner says which one is in
    // flight; it draws nothing at all when stderr is not a terminal, and erases
    // itself before anything below is printed.
    let spinner = Spinner::new(reporter.interactive());
    let narrating = Narrating::new(&transport, &spinner);
    let report = prepare_target(&narrating, settings, &loaded.config, &name, &SystemClock)?;
    drop(spinner);

    // Last, and only now: every check passed, so the file can be replaced.
    config::io::save(&path, report.config())?;

    render(&report, ssh_host, reporter)
}

fn render(report: &SetupReport, ssh_host: &str, reporter: &Reporter) -> Result<(), CliftError> {
    if reporter.json() {
        let steps: Vec<&str> = report.steps().iter().map(|step| step.label()).collect();
        let value = serde_json::json!({
            "schema_version": 1,
            "status": "ok",
            "target": ssh_host,
            "remote": report.settings().summary(),
            "storage": report.inbox().as_str(),
            "checks": steps,
            "warnings": report.warnings(),
        });
        return reporter.machine(&value).map_err(stdout_failed);
    }

    reporter.success(&format!("Storage: {}", report.inbox()));
    reporter.success("");
    for step in report.steps() {
        reporter.success(&format!("\u{2713} {}", step.label()));
    }
    for warning in report.warnings() {
        reporter.warn(warning);
    }
    reporter.success("");
    reporter.success(&format!(
        "Ready. Run: clift send --clipboard --to {ssh_host}"
    ));
    Ok(())
}

/// Reads a yes/no answer from the terminal.
///
/// The prompt goes to stderr like every other human-readable message: stdout
/// belongs to machine results, and a question typed into an agent's prompt
/// would be worse than no question at all.
fn ask(question: &str) -> Result<bool, CliftError> {
    let stdin = std::io::stdin();
    let mut input = stdin.lock();
    let mut output = std::io::stderr();
    let mut console = Console::new(&mut input, &mut output);
    console.say("");
    console.confirm(question, false)
}

fn cancelled(ssh_host: &str) -> CliftError {
    CliftError::new(
        Stage::Config,
        ErrorKind::Config,
        format!("setup of {ssh_host} was cancelled"),
    )
    .with_remedy(Remedy::new(
        "Run it again when you are ready:",
        format!("clift setup {ssh_host}"),
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
