//! `clift config path | get | set | validate`.

use crate::cli::ConfigCommand;
use crate::output::Reporter;
use clift_core::config;
use clift_core::error::{CliftError, ErrorKind, Remedy, Stage};
use std::path::Path;

/// # Errors
/// Propagates configuration failures, all of which map to exit code 20.
pub fn run(command: &ConfigCommand, reporter: &Reporter) -> Result<(), CliftError> {
    let path = config::io::default_config_path()?;
    match command {
        ConfigCommand::Path => show_path(&path, reporter),
        ConfigCommand::Get { key } => show_value(&path, key, reporter),
        ConfigCommand::Set { key, value } => set_value(&path, key, value, reporter),
        ConfigCommand::Validate => validate(&path, reporter),
    }
}

fn show_path(path: &Path, reporter: &Reporter) -> Result<(), CliftError> {
    let text = path.display().to_string();
    if reporter.json() {
        emit(
            reporter,
            &serde_json::json!({
                "schema_version": 1,
                "status": "ok",
                "path": text,
                "exists": path.exists(),
            }),
        )
    } else {
        write_result(reporter, &format!("{text}\n"))
    }
}

fn show_value(path: &Path, key: &str, reporter: &Reporter) -> Result<(), CliftError> {
    let (document, _) = parse_document(path)?;
    let value = config::edit::get(&document, key)?;

    if reporter.json() {
        emit(
            reporter,
            &serde_json::json!({
                "schema_version": 1,
                "status": "ok",
                "key": key,
                "value": value,
            }),
        )
    } else {
        write_result(reporter, &format!("{value}\n"))
    }
}

/// Sets one key in the document at `path`, validating the result before it is
/// written. Shared with the first-time setup, which saves a relay the same way
/// `clift config set relay.url` would.
///
/// # Errors
/// Fails when the document cannot be read, the key is unknown, or the edited
/// document would not load.
pub fn write_key(
    path: &Path,
    key: &str,
    value: &str,
    reporter: &Reporter,
) -> Result<(), CliftError> {
    let (mut document, notes) = parse_document(path)?;
    for note in &notes {
        reporter.warn(note);
    }

    config::edit::set(&mut document, key, value)?;
    // A file Clift writes always declares its schema version, even when the one
    // it replaced predated versioning.
    config::migrate::migrate_to_current(&mut document)?;

    let rendered = document.to_string();
    // Validated before it is written: an edit must never be able to produce a
    // file that Clift would then refuse to load.
    let loaded = config::parse(&rendered)?;
    for warning in &loaded.warnings {
        reporter.warn(warning);
    }

    config::io::save_source(path, &rendered)
}

fn set_value(path: &Path, key: &str, value: &str, reporter: &Reporter) -> Result<(), CliftError> {
    write_key(path, key, value, reporter)?;
    reporter.success(&format!("Set {key} in {}", path.display()));

    if reporter.json() {
        return emit(
            reporter,
            &serde_json::json!({
                "schema_version": 1,
                "status": "ok",
                "key": key,
                "value": value,
                "path": path.display().to_string(),
            }),
        );
    }
    Ok(())
}

fn validate(path: &Path, reporter: &Reporter) -> Result<(), CliftError> {
    let loaded = config::io::load(path)?;
    for warning in &loaded.warnings {
        reporter.warn(warning);
    }

    if reporter.json() {
        return emit(
            reporter,
            &serde_json::json!({
                "schema_version": 1,
                "status": "ok",
                "path": path.display().to_string(),
                "targets": loaded.config.targets().len(),
                "warnings": loaded.warnings,
            }),
        );
    }

    reporter.success(&format!(
        "{} is valid ({} target(s), {} warning(s))",
        path.display(),
        loaded.config.targets().len(),
        loaded.warnings.len()
    ));
    Ok(())
}

/// Reads the document the way `load` does, so that `get` and `set` see the same
/// migrated shape the rest of Clift sees.
fn parse_document(path: &Path) -> Result<(toml::Table, Vec<String>), CliftError> {
    let source = config::io::read_source(path)?;
    let mut document: toml::Table = source.parse().map_err(|error: toml::de::Error| {
        CliftError::new(
            Stage::Config,
            ErrorKind::Config,
            format!("config file is not valid TOML: {}", error.message()),
        )
        .with_source(error)
    })?;
    let outcome = config::migrate::migrate_to_current(&mut document)?;
    Ok((document, outcome.notes))
}

fn emit(reporter: &Reporter, value: &serde_json::Value) -> Result<(), CliftError> {
    reporter.machine(value).map_err(write_failure)
}

fn write_result(reporter: &Reporter, text: &str) -> Result<(), CliftError> {
    reporter.insertion_text(text).map_err(write_failure)
}

fn write_failure(error: std::io::Error) -> CliftError {
    CliftError::new(
        Stage::Internal,
        ErrorKind::Internal,
        "cannot write to stdout",
    )
    .with_source(error)
    .with_remedy(Remedy::new(
        "Check where stdout is going:",
        "clift config path",
    ))
}
