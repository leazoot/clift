//! `clift target add | list | use | rename | remove`.
//!
//! Every subcommand is the same shape: load the configuration, hand it to a
//! use case that returns a new one, write that one atomically. A rejected
//! argument therefore cannot leave the file half-edited, because the file is
//! only ever replaced whole.

use crate::cli::TargetCommand;
use crate::output::Reporter;
use clift_core::config::{self, Config};
use clift_core::error::{CliftError, ErrorKind, Stage};
use clift_core::usecase::{self, TargetSummary};
use std::path::Path;

/// # Errors
/// Propagates configuration failures, all of which map to exit code 20.
pub fn run(command: &TargetCommand, reporter: &Reporter) -> Result<(), CliftError> {
    let path = config::io::default_config_path()?;
    let loaded = config::io::load(&path)?;
    for warning in &loaded.warnings {
        reporter.warn(warning);
    }
    let current = loaded.config;

    match command {
        TargetCommand::List => show(&current, reporter),
        TargetCommand::Add { name, ssh_host } => {
            let next = usecase::add(&current, name, ssh_host.as_deref())?;
            save(&path, &next)?;
            reporter.success(&format!("Added {name}."));
            reporter.success(&format!("Verify it with: clift doctor {name}"));
            Ok(())
        }
        TargetCommand::Use { name } => {
            let next = usecase::use_default(&current, name)?;
            save(&path, &next)?;
            reporter.success(&format!("Default target is now {name}."));
            Ok(())
        }
        TargetCommand::Rename { from, to } => {
            let next = usecase::rename(&current, from, to)?;
            save(&path, &next)?;
            reporter.success(&format!("Renamed {from} to {to}."));
            Ok(())
        }
        TargetCommand::Remove { name } => {
            let removal = usecase::remove(&current, name)?;
            save(&path, &removal.config)?;
            reporter.success(&format!("Removed {name}."));
            // The attachments are the user's. Forgetting a name is not consent
            // to delete files on their server.
            reporter.success(&format!(
                "Its inbox is still on {} at {}.",
                removal.ssh_host, removal.remote_dir
            ));
            if removal.default_cleared {
                reporter.success(
                    "There is no default target now. Set one with: clift target use <name>",
                );
            }
            Ok(())
        }
    }
}

fn show(config: &Config, reporter: &Reporter) -> Result<(), CliftError> {
    let rows = usecase::list(config);

    if reporter.json() {
        let targets: Vec<serde_json::Value> = rows
            .iter()
            .map(|row| {
                serde_json::json!({
                    "name": row.name,
                    "ssh_host": row.ssh_host,
                    "default": row.is_default,
                    "last_success_at": row.last_success_at,
                })
            })
            .collect();
        return reporter
            .machine(&serde_json::json!({
                "schema_version": 1,
                "status": "ok",
                "targets": targets,
            }))
            .map_err(stdout_failed);
    }

    if rows.is_empty() {
        reporter.success("No targets configured.");
        reporter.success("Add one with: clift setup <ssh-host>");
        return Ok(());
    }

    let width = rows.iter().map(|row| row.name.len()).max().unwrap_or(0);
    for row in &rows {
        reporter.success(&render_row(row, width));
    }
    Ok(())
}

/// One `list` line: name, alias, whether it is the default, and when it last
/// worked. Nothing else -- the specification keeps key locations and tokens out of here,
/// and the way to keep them out is to have nowhere to put them.
fn render_row(row: &TargetSummary, width: usize) -> String {
    let marker = if row.is_default { "*" } else { " " };
    let last = row.last_success_at.as_deref().unwrap_or("never connected");
    format!(
        "{marker} {:width$}  {}  {last}",
        row.name,
        row.ssh_host,
        width = width
    )
}

fn save(path: &Path, config: &Config) -> Result<(), CliftError> {
    config::io::save(path, config)
}

fn stdout_failed(error: std::io::Error) -> CliftError {
    CliftError::new(
        Stage::Internal,
        ErrorKind::Internal,
        "could not write the result to stdout",
    )
    .with_source(error)
}
