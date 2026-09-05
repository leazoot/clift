//! `clift uninstall`.
//!
//! Three things are treated differently, and the differences are the point:
//!
//! - the **hotkey login item** goes, and the helper it started is stopped. It
//!   is Clift's own registration pointing at the Clift binary, so leaving it
//!   behind means the next login starts something that may no longer be there;
//! - **Clift's own configuration** stays unless `--purge`, and either way the
//!   user is told where it is;
//! - the **attachments on remote hosts** are never touched. They are the
//!   user's files on the user's servers, and uninstalling a local tool is not
//!   consent to delete them -- so their locations are reported with the one
//!   command that would clear each.
//!
//! `~/.ssh/config` is not read, not written, and not mentioned: Clift never put
//! anything in it.

use crate::output::Reporter;
use clift_core::config;
use clift_core::context::{Confirmation, confirmation_for};
use clift_core::error::{CliftError, ErrorKind, Remedy, Stage};
use clift_core::usecase::{cleanup_command, plan_uninstall};
use std::fs;
use std::io::{BufRead, IsTerminal, Write};

/// # Errors
/// Propagates configuration and filesystem failures. `--dry-run` changes
/// nothing and so cannot leave anything half-done.
pub fn run(purge: bool, yes: bool, dry_run: bool, reporter: &Reporter) -> Result<(), CliftError> {
    let path = config::io::default_config_path()?;
    let loaded = config::io::load(&path).unwrap_or_else(|_| config::ConfigLoad {
        // A configuration Clift cannot parse is still a configuration the user
        // may want removed; uninstalling must not require it to be valid.
        config: config::Config::default(),
        warnings: Vec::new(),
    });
    let plan = plan_uninstall(&loaded.config, &path, purge);

    // Read before anything is described, so that `--dry-run` names the same
    // file the real run will remove.
    let login_item = clift_inject::autostart::installed();

    reporter.success(if dry_run {
        "Would make these changes:"
    } else {
        "Removing Clift:"
    });

    match &login_item {
        Some(path) => reporter.success(&format!("remove  {}", path.display())),
        None => reporter.success("keep    no hotkey helper is registered to start at login"),
    }

    if plan.removes_config() && plan.config_exists() {
        reporter.success(&format!("remove  {}", plan.config_path().display()));
    } else if plan.config_exists() {
        reporter.success(&format!(
            "keep    {} (use --purge to delete it)",
            plan.config_path().display()
        ));
    }

    for leftovers in plan.leftovers() {
        reporter.success(&format!(
            "keep    {}:{} -- clear it yourself with: {}",
            leftovers.ssh_host,
            leftovers.remote_dir,
            cleanup_command(leftovers)
        ));
    }

    if dry_run {
        return Ok(());
    }

    if plan.removes_config() && plan.config_exists() {
        match confirmation_for(
            "deleting Clift's configuration",
            std::io::stdin().is_terminal(),
            yes,
        )? {
            Confirmation::AlreadyGiven => {}
            Confirmation::Ask => {
                if !ask("Delete Clift's configuration?")? {
                    return Err(cancelled());
                }
            }
        }
    }

    // Removing the definition also stops the helper that is running from it,
    // which is the half a user cannot do themselves once the entry is gone.
    if login_item.is_some() {
        clift_inject::autostart::uninstall()?;
    }

    if plan.removes_config() && plan.config_exists() {
        fs::remove_file(plan.config_path()).map_err(|error| {
            CliftError::new(
                Stage::Config,
                ErrorKind::Config,
                format!("cannot remove {}", plan.config_path().display()),
            )
            .with_source(error)
        })?;
        // The directory is Clift's own (`places` names it), so an empty one is
        // taken along; one that still holds something the user put there is
        // left, which is what `remove_dir` refusing a non-empty directory does.
        if let Some(directory) = plan.config_path().parent() {
            let _ = fs::remove_dir(directory);
        }
    }

    reporter.success("");
    if login_item.is_some() {
        reporter.success(
            "Done. The hotkey helper is stopped and it will not start at your next login.",
        );
    } else {
        reporter.success("Done.");
    }
    if !plan.leftovers().is_empty() {
        reporter.success("The attachments already on your hosts were left alone.");
    }
    Ok(())
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

fn cancelled() -> CliftError {
    CliftError::new(Stage::Config, ErrorKind::Config, "uninstall was cancelled").with_remedy(
        Remedy::new(
            "Keep the configuration and remove the rest:",
            "clift uninstall",
        ),
    )
}
