//! Connection probing: what works, what does not, and in OpenSSH's own words.
//!
//! A probe answers one question per line so that a user with three problems
//! learns about all three at once. It never retries and never varies the
//! arguments it uses: the specification rules out reconnecting with weaker settings, so
//! a failed check ends that line of enquiry rather than starting a second,
//! more permissive attempt.

use crate::errmap::{Symptom, classify};
use crate::proc::{SftpBatch, SshRunner};
use clift_core::error::CliftError;
use clift_core::ports::{CheckStatus, ProbeCheck, ProbeReport, TransportTarget};

/// Names of the checks a probe reports, in the order they are attempted.
pub const CHECK_SSH_CLIENT: &str = "ssh client";
pub const CHECK_SFTP_CLIENT: &str = "sftp client";
pub const CHECK_CONNECTION: &str = "connection";
pub const CHECK_HOST_KEY: &str = "host key";
pub const CHECK_AUTHENTICATION: &str = "authentication";
pub const CHECK_SFTP_SUBSYSTEM: &str = "sftp subsystem";

/// The transport adapter: everything Clift does to a remote host goes through
/// the system OpenSSH client held here.
#[derive(Debug, Clone, Default)]
pub struct OpenSshTransport {
    runner: SshRunner,
}

impl OpenSshTransport {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn with_runner(runner: SshRunner) -> Self {
        Self { runner }
    }

    pub(crate) fn runner(&self) -> &SshRunner {
        &self.runner
    }

    /// Checks that the host is reachable, authenticated and speaks SFTP.
    ///
    /// This is the body of [`clift_core::ports::Transport::probe`]; the trait
    /// is implemented once every one of its methods exists.
    ///
    /// # Errors
    /// Fails only when the probe itself could not run. A host that refuses the
    /// connection is a reported check, not an error: the caller wants the whole
    /// report, not the first problem.
    pub fn probe(&self, target: &TransportTarget) -> Result<ProbeReport, CliftError> {
        let mut report = ProbeReport::default();

        let clients_ok = self.check_clients(&mut report);
        if !clients_ok {
            // Without a client there is nothing to say about the host, and
            // guessing would put words in OpenSSH's mouth.
            for name in [
                CHECK_CONNECTION,
                CHECK_HOST_KEY,
                CHECK_AUTHENTICATION,
                CHECK_SFTP_SUBSYSTEM,
            ] {
                report
                    .checks
                    .push(not_reached(name, "the OpenSSH client is missing"));
            }
            return Ok(report);
        }

        let reachable = self.check_connection(target, &mut report)?;
        if !reachable {
            report.checks.push(not_reached(
                CHECK_SFTP_SUBSYSTEM,
                "the SSH connection failed",
            ));
            return Ok(report);
        }

        self.check_sftp_subsystem(target, &mut report)?;
        Ok(report)
    }

    fn check_clients(&self, report: &mut ProbeReport) -> bool {
        let ssh = match self.runner.ssh_version() {
            Ok(version) => {
                report
                    .checks
                    .push(pass(CHECK_SSH_CLIENT, format!("found {version}")));
                true
            }
            Err(error) => {
                report
                    .checks
                    .push(fail(CHECK_SSH_CLIENT, error.message().to_string()));
                false
            }
        };
        let sftp = match self.runner.sftp_present() {
            Ok(()) => {
                report
                    .checks
                    .push(pass(CHECK_SFTP_CLIENT, "found".to_string()));
                true
            }
            Err(error) => {
                report
                    .checks
                    .push(fail(CHECK_SFTP_CLIENT, error.message().to_string()));
                false
            }
        };
        ssh && sftp
    }

    /// Runs one `ssh <host> true`, then reports what its outcome proves about
    /// reachability, the host key and authentication.
    fn check_connection(
        &self,
        target: &TransportTarget,
        report: &mut ProbeReport,
    ) -> Result<bool, CliftError> {
        let outcome = self.runner.run_ssh(target, "true")?;
        if outcome.succeeded() {
            report.checks.push(pass(
                CHECK_CONNECTION,
                format!("{} is reachable", target.ssh_host()),
            ));
            report.checks.push(pass(
                CHECK_HOST_KEY,
                "the host key matches the one in known_hosts".to_string(),
            ));
            report
                .checks
                .push(pass(CHECK_AUTHENTICATION, "accepted".to_string()));
            return Ok(true);
        }

        let stderr = outcome.stderr.trim();
        match classify(stderr) {
            Symptom::HostKeyChanged => {
                report.checks.push(pass(
                    CHECK_CONNECTION,
                    format!("{} answered", target.ssh_host()),
                ));
                report.checks.push(fail(
                    CHECK_HOST_KEY,
                    format!(
                        "the host key has changed since it was recorded in known_hosts. \
                         OpenSSH reported:\n{stderr}"
                    ),
                ));
                report.checks.push(not_reached(
                    CHECK_AUTHENTICATION,
                    "the host key was rejected",
                ));
            }
            Symptom::HostKeyUnknown => {
                report.checks.push(pass(
                    CHECK_CONNECTION,
                    format!("{} answered", target.ssh_host()),
                ));
                report.checks.push(fail(
                    CHECK_HOST_KEY,
                    format!("the host key is not in known_hosts. OpenSSH reported: {stderr}"),
                ));
                report.checks.push(not_reached(
                    CHECK_AUTHENTICATION,
                    "the host key was rejected",
                ));
            }
            Symptom::AuthenticationRejected => {
                report.checks.push(pass(
                    CHECK_CONNECTION,
                    format!("{} answered", target.ssh_host()),
                ));
                report.checks.push(pass(
                    CHECK_HOST_KEY,
                    "verified before authentication was attempted".to_string(),
                ));
                report
                    .checks
                    .push(fail(CHECK_AUTHENTICATION, stderr.to_string()));
            }
            // The remaining symptoms describe SFTP operations, which
            // `ssh host true` does not perform. If one ever turns up here it
            // belongs with the failures this step cannot attribute.
            Symptom::Unreachable
            | Symptom::SftpSubsystemMissing
            | Symptom::RemotePermissionDenied
            | Symptom::RemoteMissing
            | Symptom::TransferFailed
            | Symptom::Unrecognised => {
                // Attributing an unrecognised failure to the first step is the
                // least misleading option, and the detail carries OpenSSH's own
                // words rather than a paraphrase of them.
                report
                    .checks
                    .push(fail(CHECK_CONNECTION, stderr.to_string()));
                report
                    .checks
                    .push(not_reached(CHECK_HOST_KEY, "the connection failed"));
                report
                    .checks
                    .push(not_reached(CHECK_AUTHENTICATION, "the connection failed"));
            }
        }
        Ok(false)
    }

    fn check_sftp_subsystem(
        &self,
        target: &TransportTarget,
        report: &mut ProbeReport,
    ) -> Result<(), CliftError> {
        let mut batch = SftpBatch::new();
        batch.push("pwd", &[])?;
        let outcome = self.runner.run_sftp(target, &batch)?;
        if outcome.succeeded() {
            report
                .checks
                .push(pass(CHECK_SFTP_SUBSYSTEM, "available".to_string()));
            return Ok(());
        }

        let stderr = outcome.stderr.trim();
        let detail = if classify(stderr) == Symptom::SftpSubsystemMissing {
            format!(
                "the server accepted the login but does not offer the SFTP subsystem. \
                 OpenSSH reported: {stderr}"
            )
        } else {
            stderr.to_string()
        };
        report.checks.push(ProbeCheck {
            name: CHECK_SFTP_SUBSYSTEM.to_string(),
            status: CheckStatus::Fail,
            detail,
        });
        Ok(())
    }
}

fn pass(name: &str, detail: String) -> ProbeCheck {
    ProbeCheck {
        name: name.to_string(),
        status: CheckStatus::Pass,
        detail,
    }
}

fn fail(name: &str, detail: String) -> ProbeCheck {
    ProbeCheck {
        name: name.to_string(),
        status: CheckStatus::Fail,
        detail,
    }
}

/// A check that was not attempted because an earlier one failed.
///
/// Reported as a warning rather than silently omitted: a missing line reads as
/// "fine", and the point of a probe is that nothing is left implied. The port
/// has no separate "skipped" status, and the same question was settled for
/// `doctor` in favour of a warning.
fn not_reached(name: &str, because: &str) -> ProbeCheck {
    ProbeCheck {
        name: name.to_string(),
        status: CheckStatus::Warn,
        detail: format!("not checked: {because}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_not_reached_check_says_so_rather_than_being_left_out() {
        let check = not_reached(CHECK_AUTHENTICATION, "the connection failed");
        assert_eq!(check.status, CheckStatus::Warn);
        assert!(check.detail.starts_with("not checked:"), "{check:?}");
    }
}
