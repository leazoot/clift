//! `clift doctor [target]`.
//!
//! Renders the thirteen checks. The command's exit status is what a script
//! reads, so it follows the report: any failure is exit code 30, warnings are
//! not failures. An installation with nothing configured for Fast Mode is
//! incomplete, not broken, and telling a user otherwise trains them to ignore
//! the tool.

use crate::output::Reporter;
use crate::progress::{Narrating, Spinner};
use crate::relay;
use crate::system::SystemRandomness;
use clift_core::config::{self, ConfigLoad};
use clift_core::diagnostics::{
    CheckName, ConfigState, DoctorCheck, DoctorReport, Environment, InjectionFacts, InjectionState,
    LocalFacts, RelayProbe, diagnose, dto,
};
use clift_core::domain::TargetName;
use clift_core::error::{CliftError, ErrorKind, Stage};
use clift_core::ports::TransportTarget;

/// # Errors
/// Returns an internal error when a check failed, so that the exit status
/// reflects the report. The report itself is always printed first.
pub fn run(target: Option<&str>, reporter: &Reporter) -> Result<(), CliftError> {
    let path = config::io::default_config_path()?;
    let exists = path.exists();

    // A configuration Clift cannot read must not stop the other twelve checks:
    // that user is precisely the one who needs them.
    let (loaded, config_error) = match config::io::load(&path) {
        Ok(loaded) => (Some(loaded), None),
        Err(error) => (None, Some(error.message().to_string())),
    };

    let chosen = resolve_target(loaded.as_ref(), target)?;
    let remote_dir = chosen.as_ref().and_then(|host| {
        loaded.as_ref().and_then(|loaded| {
            loaded
                .config
                .targets()
                .values()
                .find(|entry| entry.ssh_host() == host)
                .map(|entry| entry.remote_dir().to_string())
        })
    });
    // Universal Mode's half of the report. A relay that is configured but
    // unusable is a real finding, so the settings are resolved even when they
    // are wrong -- the failure becomes a failed check rather than a refusal to
    // run `doctor` at all, which is the one command a broken setup needs.
    let (relay_settings, relay_note) = match relay::is_configured(
        loaded
            .as_ref()
            .map_or(&clift_core::config::Config::default(), |loaded| {
                &loaded.config
            }),
    ) {
        true => match relay::settings(
            loaded
                .as_ref()
                .map_or(&clift_core::config::Config::default(), |loaded| {
                    &loaded.config
                }),
        ) {
            Ok(settings) => (Some(settings), None),
            Err(error) => (None, Some(error.message().to_string())),
        },
        false => (None, None),
    };
    if let Some(note) = &relay_note {
        reporter.warn(&format!("the configured relay cannot be used: {note}"));
    }
    let relay_client = relay_settings.as_ref().map(clift_relay::HttpRelay::new);

    let transport = crate::system::transport(
        loaded
            .as_ref()
            .map_or(&clift_core::config::Config::default(), |loaded| {
                &loaded.config
            }),
        reporter,
    );
    // Thirteen checks, several of them a round trip each. Without this, doctor
    // looks hung for half a minute on a distant host.
    let spinner = Spinner::new(reporter.interactive());
    let narrating = Narrating::new(&transport, &spinner);
    // Read once, here, because that is the check. On a platform Clift cannot
    // read the clipboard on there is nothing to hand over, and the check says
    // so rather than reporting a failure of the user's installation.
    let reader = clift_clipboard::SystemClipboard::new();
    let environment = Environment {
        facts: LocalFacts {
            platform: env!("CLIFT_TARGET").to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
        },
        // `ssh -G` is local and instant, so it is not worth announcing.
        ssh_config: &transport,
        remote: &narrating,
        upload: &narrating,
        clipboard: clift_clipboard::IS_SUPPORTED.then_some(&reader),
        remote_dir: remote_dir.clone(),
        injection: Some(injection_facts()),
        relay: match (&relay_settings, &relay_client) {
            (Some(settings), Some(client)) => Some(RelayProbe {
                settings,
                relay: client,
                random: &SystemRandomness,
            }),
            _ => None,
        },
        config: ConfigState {
            exists,
            version: loaded.as_ref().map_or(0, |loaded| loaded.config.version()),
            supported: clift_core::config::schema::SUPPORTED_VERSION,
            warnings: loaded
                .as_ref()
                .map(|loaded| loaded.warnings.clone())
                .unwrap_or_default(),
            error: config_error,
        },
    };

    let host = chosen.as_ref().map(TransportTarget::new);
    let report = diagnose(&environment, host.as_ref());

    render(&report, chosen.as_deref(), reporter)?;

    if report.passed() {
        Ok(())
    } else {
        Err(CliftError::new(
            Stage::Internal,
            ErrorKind::Internal,
            format!(
                "{} of {} checks failed",
                report.failures(),
                report.checks.len()
            ),
        ))
    }
}

/// What this machine says about typing into the focused window.
///
/// Gathered here rather than behind a port because there is nothing to
/// perform: three questions are asked of the operating system once and the
/// answers are handed to `clift-core`, which decides what they mean. The
/// judgement is the part that is a business rule, and it stays there.
fn injection_facts() -> InjectionFacts {
    let state = match clift_inject::availability() {
        clift_inject::Availability::Ready => InjectionState::Ready,
        // The adapter's own wording is for the user who just asked for
        // `--inject`; `doctor` names the binary instead, because in a report
        // "which binary" is the question that is actually being asked.
        clift_inject::Availability::NeedsPermission(_) => InjectionState::NeedsPermission {
            command: clift_inject::permission_command().map(str::to_string),
        },
        clift_inject::Availability::Unsupported(reason) => InjectionState::Unsupported { reason },
    };
    InjectionFacts {
        state,
        program: std::env::current_exe()
            .ok()
            .map(|path| path.display().to_string()),
        helper: clift_inject::autostart::registered_program()
            .map(|path| path.display().to_string()),
    }
}

/// The target to diagnose: the one named, else the configured default.
///
/// An explicitly named target that does not exist is an error. No target at all
/// is not: `doctor` on a fresh machine is exactly when its advice is worth most.
fn resolve_target(
    loaded: Option<&ConfigLoad>,
    requested: Option<&str>,
) -> Result<Option<String>, CliftError> {
    let Some(loaded) = loaded else {
        return Ok(requested.map(str::to_string));
    };
    match requested {
        Some(name) => {
            let parsed = TargetName::new(name)
                .map_err(|error| error.into_clift(Stage::Config, ErrorKind::Config))?;
            match loaded.config.target(&parsed) {
                Some(target) => Ok(Some(target.ssh_host().to_string())),
                None => Err(CliftError::new(
                    Stage::Config,
                    ErrorKind::Config,
                    format!("there is no target called {name}"),
                )
                .with_remedy(clift_core::error::Remedy::new(
                    "See what is configured:",
                    "clift target list",
                ))),
            }
        }
        None => Ok(loaded
            .config
            .default_target()
            .and_then(|name| loaded.config.target(name))
            .map(|target| target.ssh_host().to_string())),
    }
}

fn render(
    report: &DoctorReport,
    host: Option<&str>,
    reporter: &Reporter,
) -> Result<(), CliftError> {
    if reporter.json() {
        let document = dto::doctor(report, host);
        let value = serde_json::to_value(&document).map_err(|error| {
            CliftError::new(
                Stage::Internal,
                ErrorKind::Internal,
                "could not build the doctor document",
            )
            .with_source(error)
        })?;
        return reporter.machine(&value).map_err(stdout_failed);
    }

    let width = CheckName::ALL
        .iter()
        .map(|name| name.as_str().len())
        .max()
        .unwrap_or(0);
    for check in &report.checks {
        reporter.success(&line(check, width));
    }
    for check in &report.checks {
        if let Some(remedy) = &check.remedy {
            reporter.success("");
            reporter.success(&format!(
                "{}: {}",
                check.name.as_str(),
                remedy.description()
            ));
            reporter.success(&format!("  {}", remedy.command()));
        }
    }
    Ok(())
}

fn line(check: &DoctorCheck, width: usize) -> String {
    format!(
        "{:width$}  {:<4}  {}",
        check.name.as_str(),
        dto::status_word(check.status),
        check.detail,
        width = width
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
