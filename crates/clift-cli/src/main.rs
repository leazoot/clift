//! Command line entry point for Clift.
//!
//! This binary is the composition root: it parses arguments, constructs the
//! concrete adapters, injects them into `clift-core` use cases and renders the
//! result. It holds no business rules of its own, and in particular it never
//! defines an exit code: that mapping lives in `clift-core`.

#![forbid(unsafe_code)]

mod cli;
mod cmd;
mod dto;
mod output;
mod progress;
mod prompt;
mod relay;
mod system;

use clap::Parser;
use cli::{Cli, Command};
use clift_core::error::{CliftError, ErrorKind};
use output::Reporter;
use std::process::ExitCode;

/// Version, commit and target triple are all three required by the specification: a user
/// reporting a bug must be able to identify the exact binary they ran.
fn version_line() -> String {
    format!(
        "clift {} ({} {})",
        env!("CARGO_PKG_VERSION"),
        env!("CLIFT_COMMIT"),
        env!("CLIFT_TARGET"),
    )
}

fn main() -> ExitCode {
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(error) => return render_usage_error(&error),
    };

    let reporter = Reporter::new(cli.json, cli.verbose, cli.debug);

    if cli.version {
        // Identifying the running binary is a machine-readable result, so it is
        // one of the few things that legitimately belongs on stdout.
        let written = if reporter.json() {
            reporter.machine(&serde_json::json!({
                "schema_version": 1,
                "status": "ok",
                "version": env!("CARGO_PKG_VERSION"),
                "commit": env!("CLIFT_COMMIT"),
                "target": env!("CLIFT_TARGET"),
            }))
        } else {
            reporter.insertion_text(&format!("{}\n", version_line()))
        };
        if written.is_err() {
            return ExitCode::from(ErrorKind::Internal.exit_code().as_u8());
        }
        return ExitCode::SUCCESS;
    }

    // Ctrl+C ends a Rust program without unwinding, so nothing's `Drop` runs
    // and a clipboard image would be left on disk. Failing to install this is
    // not a reason to refuse to run: the ordinary paths still clean up.
    #[cfg(unix)]
    if let Err(error) = clift_core::runtime::remove_scratch_files_on_signal() {
        reporter.verbose(&format!("interrupt cleanup unavailable: {error}"));
    }

    match run(&cli, &reporter) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            reporter.error(&error);
            ExitCode::from(error.exit_code().as_u8())
        }
    }
}

fn run(cli: &Cli, reporter: &Reporter) -> Result<(), CliftError> {
    let Some(command) = &cli.command else {
        // No subcommand is not an error: print the usage summary and stop.
        let _ = <Cli as clap::CommandFactory>::command().print_help();
        return Ok(());
    };

    reporter.verbose(&format!("running {}", command_name(command)));

    match command {
        Command::Config { command } => cmd::config::run(command, reporter),
        Command::Setup { ssh_host, yes } => cmd::setup::run(ssh_host.as_deref(), *yes, reporter),
        Command::Target { command } => cmd::target::run(command, reporter),
        Command::Doctor { target } => cmd::doctor::run(target.as_deref(), reporter),
        Command::Status => cmd::status::run(reporter),
        Command::Uninstall {
            purge,
            yes,
            dry_run,
        } => cmd::uninstall::run(*purge, *yes, *dry_run, reporter),
        Command::Paste {
            mode,
            to,
            copy,
            inject,
        } => cmd::paste::run(
            mode.as_deref(),
            to.as_deref(),
            *copy,
            *inject,
            &clift_clipboard::SystemClipboard::new(),
            reporter,
        ),
        Command::Fetch {
            token,
            print_path,
            copy,
        } => cmd::fetch::run(token, *print_path, *copy, reporter),
        Command::Copy { files } => cmd::copy::run(files, reporter),
        Command::Hotkey {
            key,
            install,
            uninstall,
        } => cmd::hotkey::run(key.as_deref(), *install, *uninstall, reporter),
        Command::Clean {
            target,
            all,
            older_than,
            yes,
            dry_run,
        } => cmd::clean::run(
            target.as_deref(),
            *all,
            older_than.as_deref(),
            *yes,
            *dry_run,
            reporter,
        ),
        Command::Send {
            files,
            clipboard,
            to,
            copy,
            format,
        } => cmd::send::run(
            files,
            *clipboard,
            to.as_deref(),
            *copy,
            format.as_deref(),
            reporter,
        ),
    }
}

fn command_name(command: &Command) -> &'static str {
    match command {
        Command::Setup { .. } => "setup",
        Command::Send { .. } => "send",
        Command::Paste { .. } => "paste",
        Command::Fetch { .. } => "fetch",
        Command::Copy { .. } => "copy",
        Command::Hotkey { .. } => "hotkey",
        Command::Target { .. } => "target",
        Command::Doctor { .. } => "doctor",
        Command::Status => "status",
        Command::Clean { .. } => "clean",
        Command::Config { .. } => "config",
        Command::Uninstall { .. } => "uninstall",
    }
}

/// Renders a clap failure.
///
/// `--help` and `--version` are successful outcomes and go to stdout, which is
/// what every other CLI does and what shells expect. A genuine usage error is
/// reported on stderr and mapped onto the specification's configuration exit code: adding
/// an eleventh exit code for it is not allowed.
fn render_usage_error(error: &clap::Error) -> ExitCode {
    use clap::error::ErrorKind as ClapKind;

    match error.kind() {
        ClapKind::DisplayHelp | ClapKind::DisplayHelpOnMissingArgumentOrSubcommand => {
            let _ = error.print();
            ExitCode::SUCCESS
        }
        _ => {
            // `clap::Error::print` writes usage errors to stderr already.
            let _ = error.print();
            ExitCode::from(ErrorKind::Config.exit_code().as_u8())
        }
    }
}
