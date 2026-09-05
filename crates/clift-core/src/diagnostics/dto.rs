//! The shapes Clift promises to third parties.
//!
//! These types exist so that renaming a field in the domain cannot silently
//! change the JSON a third party depends on. They are written by hand and
//! versioned: adding a field is allowed, removing or renaming one requires
//! `schema_version` to go up.
//!
//! The specification is enforced here by omission. There is no field for a key location,
//! a token or an attachment's contents, so no amount of careless rendering can
//! produce one.

use super::{DoctorReport, LocalFacts};
use crate::config::Config;
use crate::ports::CheckStatus;
use crate::universal::RelaySettings;
use serde::Serialize;

/// The version of the JSON contract these types express.
pub const SCHEMA_VERSION: u32 = 1;

/// `clift status --json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct StatusDto {
    pub schema_version: u32,
    pub status: &'static str,
    pub version: String,
    pub config_path: String,
    pub default_target: Option<String>,
    pub targets: Vec<StatusTargetDto>,
    /// Which mode a bare `clift paste` would use, resolved the way the command
    /// resolves it. Added in v2.0.
    pub mode: &'static str,
    /// The relay, when one is configured. Added in v2.0.
    ///
    /// A relay has no credential and none can be configured, so there is
    /// nothing here the specification would object to -- but the field is still only the
    /// URL and the two limits, because "everything we know" is not a reason to
    /// print everything we know.
    pub relay: Option<StatusRelayDto>,
}

/// The relay, as `status` reports it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct StatusRelayDto {
    pub url: String,
    pub max_bytes: u64,
    pub ttl_seconds: u64,
}

/// One configured target, as `status` and `target list` report it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct StatusTargetDto {
    pub name: String,
    pub ssh_host: String,
    pub default: bool,
    pub last_success_at: Option<String>,
}

/// `clift doctor --json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DoctorDto {
    pub schema_version: u32,
    pub status: &'static str,
    pub target: Option<String>,
    pub checks: Vec<DoctorCheckDto>,
    pub failures: usize,
    pub warnings: usize,
}

/// One check line.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DoctorCheckDto {
    pub name: &'static str,
    pub status: &'static str,
    pub detail: String,
    /// The single command that would address this check, when it needs one.
    pub remedy: Option<String>,
}

/// Builds the `status` document.
#[must_use]
pub fn status(
    config: &Config,
    facts: &LocalFacts,
    config_path: &str,
    relay: Option<&RelaySettings>,
) -> StatusDto {
    let default = config.default_target();
    StatusDto {
        schema_version: SCHEMA_VERSION,
        status: "ok",
        version: facts.version.clone(),
        config_path: config_path.to_string(),
        default_target: default.map(|name| name.as_str().to_string()),
        targets: config
            .targets()
            .iter()
            .map(|(name, target)| StatusTargetDto {
                name: name.as_str().to_string(),
                ssh_host: target.ssh_host().to_string(),
                default: default == Some(name),
                last_success_at: target.last_success_at().map(str::to_string),
            })
            .collect(),
        // Resolved, not read from the file. The environment can supply a
        // relay too, and a status that disagreed with what `clift paste` would
        // do is worse than no status at all.
        mode: config.mode_with_relay(relay.is_some()).as_str(),
        relay: relay.map(|relay| StatusRelayDto {
            url: relay.url().to_string(),
            max_bytes: relay.max_object_bytes(),
            ttl_seconds: relay.ttl().as_secs(),
        }),
    }
}

/// Builds the `doctor` document.
#[must_use]
pub fn doctor(report: &DoctorReport, target: Option<&str>) -> DoctorDto {
    DoctorDto {
        schema_version: SCHEMA_VERSION,
        status: if report.passed() { "ok" } else { "failed" },
        target: target.map(str::to_string),
        checks: report
            .checks
            .iter()
            .map(|check| DoctorCheckDto {
                name: check.name.as_str(),
                status: status_word(check.status),
                detail: check.detail.clone(),
                remedy: check
                    .remedy
                    .as_ref()
                    .map(|remedy| remedy.command().to_string()),
            })
            .collect(),
        failures: report.failures(),
        warnings: report.warnings(),
    }
}

/// The three words `status` may take in the JSON. Stable: a plugin switches on
/// them.
#[must_use]
pub const fn status_word(status: CheckStatus) -> &'static str {
    match status {
        CheckStatus::Pass => "pass",
        CheckStatus::Warn => "warn",
        CheckStatus::Fail => "fail",
    }
}
