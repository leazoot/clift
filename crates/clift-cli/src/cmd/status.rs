//! `clift status`.
//!
//! A read-only summary: what is configured, which target is the default, when
//! each last worked, and which binary this is.
//!
//! The specification draws the line here. Nothing this command can reach holds a key
//! location, a token or an attachment, because [`clift_core::diagnostics::dto`]
//! has no field for any of them.

use crate::output::Reporter;
use crate::relay;
use clift_core::config;
use clift_core::diagnostics::{LocalFacts, StatusDto, dto};
use clift_core::error::{CliftError, ErrorKind, Stage};
use std::path::Path;

/// # Errors
/// Fails only when the configuration cannot be located or read.
pub fn run(reporter: &Reporter) -> Result<(), CliftError> {
    let path = config::io::default_config_path()?;
    let loaded = config::io::load(&path)?;
    for warning in &loaded.warnings {
        reporter.warn(warning);
    }

    let facts = LocalFacts {
        platform: env!("CLIFT_TARGET").to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
    };
    // Resolved through the same path `paste` uses, so this command reports what
    // would actually happen rather than what the file alone says. A relay that
    // is configured but unusable is reported as no relay, with the reason on
    // stderr: `status` is a summary, and a broken setting belongs in `doctor`.
    let settings = match relay::settings(&loaded.config) {
        Ok(settings) => Some(settings),
        Err(error) if relay::is_configured(&loaded.config) => {
            reporter.warn(&format!("the configured relay cannot be used: {error}"));
            None
        }
        Err(_) => None,
    };
    let report = dto::status(
        &loaded.config,
        &facts,
        &path.display().to_string(),
        settings.as_ref(),
    );

    if reporter.json() {
        let value = serde_json::to_value(&report).map_err(serialisation_failed)?;
        return reporter.machine(&value).map_err(stdout_failed);
    }
    render(&report, &path, reporter);
    Ok(())
}

fn render(report: &StatusDto, path: &Path, reporter: &Reporter) {
    reporter.success(&format!("clift {}", report.version));
    reporter.success(&format!("Config:  {}", path.display()));
    reporter.success(&format!("Mode:    {}", report.mode));
    match &report.relay {
        Some(relay) => reporter.success(&format!(
            "Relay:   {} (up to {} bytes, {}s)",
            relay.url, relay.max_bytes, relay.ttl_seconds
        )),
        // Said rather than left blank: without a relay, Universal Mode is not
        // available, and a user wondering why deserves to see that here.
        None => reporter.success("Relay:   none (Universal Mode unavailable)"),
    }

    if report.targets.is_empty() {
        reporter.success("");
        reporter.success("No targets configured.");
        // The next step, not just the absence of one: a status that only says
        // "nothing here" leaves the user to guess what to do about it.
        reporter.success("Set one up with: clift setup <ssh-host>");
        return;
    }

    reporter.success("");
    let width = report
        .targets
        .iter()
        .map(|target| target.name.len())
        .max()
        .unwrap_or(0);
    for target in &report.targets {
        let marker = if target.default { "*" } else { " " };
        let last = target
            .last_success_at
            .as_deref()
            .unwrap_or("never connected");
        reporter.success(&format!(
            "{marker} {:width$}  {}  {last}",
            target.name,
            target.ssh_host,
            width = width
        ));
    }

    if report.default_target.is_none() {
        reporter.success("");
        reporter.success("No default target.");
        reporter.success("Choose one with: clift target use <name>");
    }
}

fn serialisation_failed(error: serde_json::Error) -> CliftError {
    CliftError::new(
        Stage::Internal,
        ErrorKind::Internal,
        "could not build the status document",
    )
    .with_source(error)
}

fn stdout_failed(error: std::io::Error) -> CliftError {
    CliftError::new(
        Stage::Internal,
        ErrorKind::Internal,
        "could not write the result to stdout",
    )
    .with_source(error)
}
