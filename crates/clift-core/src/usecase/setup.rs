//! Preparing one host so that sending to it will work.
//!
//! The specification. The order is the requirement again: the configuration is written
//! **last**, after every check has passed, so that a host which turns out not
//! to work leaves no trace in `config.toml`. A half-configured target is worse
//! than no target at all -- the user would have something that looks set up and
//! fails at the moment they actually need it.
//!
//! Nothing here installs anything on the remote host, and nothing here touches
//! the user's SSH configuration. Clift's job is to use what is already there.

use crate::config::{Config, DEFAULT_REMOTE_DIR, Target};
use crate::context::SshHostSettings;
use crate::domain::{RemotePath, TargetName};
use crate::error::{CliftError, ErrorKind, Remedy, Stage};
use crate::ports::{CheckStatus, Clock, RemoteFs, RemoteUpload, TransportTarget};
use crate::staging::{ensure_inbox, verify_round_trip};

/// One step of `setup`, as the user sees it.
///
/// The four steps are the specification's four ticks, in order. They are values rather
/// than printed lines because `clift-core` does no rendering.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SetupStep {
    SshConnection,
    SftpSubsystem,
    PrivateInbox,
    UploadAndCleanup,
}

impl SetupStep {
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            SetupStep::SshConnection => "SSH connection",
            SetupStep::SftpSubsystem => "SFTP subsystem",
            SetupStep::PrivateInbox => "Private inbox",
            SetupStep::UploadAndCleanup => "Upload and cleanup",
        }
    }
}

/// Everything `setup` learned, and the configuration it would like written.
///
/// The configuration is returned rather than saved: writing it is local IO the
/// CLI performs, and returning it keeps "we got all the way to the end" and
/// "the file changed" as two separate events.
#[derive(Debug, Clone)]
pub struct SetupReport {
    settings: SshHostSettings,
    remote_home: RemotePath,
    inbox: RemotePath,
    steps: Vec<SetupStep>,
    warnings: Vec<String>,
    config: Config,
}

impl SetupReport {
    #[must_use]
    pub fn settings(&self) -> &SshHostSettings {
        &self.settings
    }

    #[must_use]
    pub fn remote_home(&self) -> &RemotePath {
        &self.remote_home
    }

    /// Where attachments will be staged. Shown as `Storage:` in the specification.
    #[must_use]
    pub fn inbox(&self) -> &RemotePath {
        &self.inbox
    }

    /// The steps that passed, in the order they were performed.
    #[must_use]
    pub fn steps(&self) -> &[SetupStep] {
        &self.steps
    }

    /// Things the user should know but which did not stop the setup, such as
    /// the host nominating a cache directory Clift declined to use.
    #[must_use]
    pub fn warnings(&self) -> &[String] {
        &self.warnings
    }

    /// The configuration to write, once the caller is ready to write it.
    #[must_use]
    pub fn config(&self) -> &Config {
        &self.config
    }
}

/// Verifies a host end to end and returns the configuration that would record it.
///
/// # Errors
/// Fails at the first step that does not work, naming the step. Nothing is
/// written to the configuration on any failure path, because nothing is written
/// here at all.
pub fn prepare_target<T>(
    transport: &T,
    settings: SshHostSettings,
    existing: &Config,
    name: &TargetName,
    clock: &dyn Clock,
) -> Result<SetupReport, CliftError>
where
    T: RemoteFs + RemoteUpload,
{
    let target = TransportTarget::new(settings.alias());
    let mut steps = Vec::with_capacity(4);

    let report = transport.probe(&target)?;
    check_passed(&report, "connection", SetupStep::SshConnection, &target)?;
    steps.push(SetupStep::SshConnection);
    check_passed(&report, "sftp subsystem", SetupStep::SftpSubsystem, &target)?;
    steps.push(SetupStep::SftpSubsystem);

    // A target being set up has no configured location yet: `setup` is what
    // writes one, and it writes the default.
    let location = ensure_inbox(transport, &target, None)?;
    steps.push(SetupStep::PrivateInbox);

    verify_round_trip(transport, transport, &target, location.root())?;
    steps.push(SetupStep::UploadAndCleanup);

    let entry = Target::new(settings.alias(), DEFAULT_REMOTE_DIR)
        .with_remote_home(location.home().clone())
        .with_last_success_at(crate::calendar::format_timestamp(clock.now()));

    let mut config = existing.with_target(name.clone(), entry);
    // The first host set up becomes the default. Later ones do not silently
    // steal it: the specification forbids guessing which host the user meant, and quietly
    // moving the default is how a send ends up on the wrong machine.
    if config.default_target().is_none() {
        config = config.with_default_target(name.clone());
    }

    Ok(SetupReport {
        settings,
        remote_home: location.home().clone(),
        inbox: location.root().clone(),
        steps,
        warnings: location.warning().into_iter().collect(),
        config,
    })
}

fn check_passed(
    report: &crate::ports::ProbeReport,
    check: &str,
    step: SetupStep,
    target: &TransportTarget,
) -> Result<(), CliftError> {
    let failed = report
        .checks
        .iter()
        .find(|entry| entry.name == check && entry.status == CheckStatus::Fail);
    match failed {
        None => Ok(()),
        Some(entry) => Err(step_failed(target, step, &entry.detail)),
    }
}

fn step_failed(target: &TransportTarget, step: SetupStep, detail: &str) -> CliftError {
    let host = target.ssh_host();
    CliftError::new(
        Stage::Connect,
        match step {
            SetupStep::SshConnection | SetupStep::SftpSubsystem => ErrorKind::SshConnection,
            SetupStep::PrivateInbox => ErrorKind::RemoteDirectory,
            SetupStep::UploadAndCleanup => ErrorKind::Transfer,
        },
        format!("{} failed on {host}: {detail}", step.label()),
    )
    .with_remedy(Remedy::new(
        "Check the connection by hand:",
        format!("ssh {host}"),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::parse_effective_config;
    use crate::testing::{FakeClock, RecordingTransport, TransportCall};

    fn settings() -> SshHostSettings {
        parse_effective_config("core", "user dev\nhostname 192.0.2.10\nport 2222\n")
            .unwrap_or_else(|error| panic!("{error}"))
    }

    fn name() -> TargetName {
        TargetName::new("core").unwrap_or_else(|error| panic!("{error}"))
    }

    fn clock() -> FakeClock {
        FakeClock::at_unix_seconds(1_788_093_240)
    }

    #[test]
    fn a_working_host_passes_all_four_steps_and_produces_a_saveable_config() {
        let transport = RecordingTransport::new("/home/dev");
        let report = prepare_target(
            &transport,
            settings(),
            &Config::default(),
            &name(),
            &clock(),
        )
        .unwrap();

        assert_eq!(
            report.steps(),
            [
                SetupStep::SshConnection,
                SetupStep::SftpSubsystem,
                SetupStep::PrivateInbox,
                SetupStep::UploadAndCleanup,
            ]
        );
        assert_eq!(report.remote_home().as_str(), "/home/dev");
        assert_eq!(report.inbox().as_str(), "/home/dev/.cache/clift/inbox");

        let saved = report.config();
        let target = saved.target(&name()).expect("the target was recorded");
        assert_eq!(target.ssh_host(), "core");
        assert_eq!(target.remote_dir(), DEFAULT_REMOTE_DIR);
        assert_eq!(
            target.remote_home().map(RemotePath::as_str),
            Some("/home/dev")
        );
        assert_eq!(target.last_success_at(), Some("2026-08-30T12:34:00Z"));
        assert_eq!(saved.default_target(), Some(&name()));
    }

    /// The self-check must not leave its own file behind.
    #[test]
    fn the_test_file_is_uploaded_and_then_removed_again() {
        let transport = RecordingTransport::new("/home/dev");
        prepare_target(
            &transport,
            settings(),
            &Config::default(),
            &name(),
            &clock(),
        )
        .unwrap();

        let path = "/home/dev/.cache/clift/inbox/clift-selfcheck".to_string();
        let calls = transport.calls();
        let uploaded = calls
            .iter()
            .position(|call| matches!(call, TransportCall::UploadAtomic { destination, .. } if *destination == path));
        let removed = calls.iter().position(
            |call| matches!(call, TransportCall::Remove { path: removed } if *removed == path),
        );

        let uploaded = uploaded.expect("the self check must upload something");
        let removed = removed.expect("the self check must remove what it uploaded");
        assert!(removed > uploaded, "removed before it was uploaded");
    }

    /// AC 2: nothing is written on a failure path, and there is nothing to
    /// write, because the configuration only comes back on success.
    #[test]
    fn a_failing_self_check_produces_no_configuration_at_all() {
        let transport = RecordingTransport::new("/home/dev");
        transport.fail_upload_of("/home/dev/.cache/clift/inbox/clift-selfcheck");

        let error = prepare_target(
            &transport,
            settings(),
            &Config::default(),
            &name(),
            &clock(),
        )
        .expect_err("the self check was made to fail");
        assert_eq!(error.exit_code().as_u8(), 23);
    }

    /// An existing default target is not quietly moved to the new host.
    #[test]
    fn setting_up_a_second_host_does_not_steal_the_default() {
        let transport = RecordingTransport::new("/home/dev");
        let first = prepare_target(
            &transport,
            settings(),
            &Config::default(),
            &name(),
            &clock(),
        )
        .unwrap();

        let second_name = TargetName::new("laptop").unwrap_or_else(|error| panic!("{error}"));
        let second_settings =
            parse_effective_config("laptop", "user dev\nhostname 192.0.2.20\nport 22\n")
                .unwrap_or_else(|error| panic!("{error}"));
        let second = prepare_target(
            &transport,
            second_settings,
            first.config(),
            &second_name,
            &clock(),
        )
        .unwrap();

        assert_eq!(second.config().targets().len(), 2);
        assert_eq!(
            second.config().default_target(),
            Some(&name()),
            "the default must stay where the user put it"
        );
    }

    #[test]
    fn the_four_step_labels_are_the_ones_prd_16_2_prints() {
        assert_eq!(
            [
                SetupStep::SshConnection.label(),
                SetupStep::SftpSubsystem.label(),
                SetupStep::PrivateInbox.label(),
                SetupStep::UploadAndCleanup.label(),
            ],
            [
                "SSH connection",
                "SFTP subsystem",
                "Private inbox",
                "Upload and cleanup"
            ]
        );
    }
}
