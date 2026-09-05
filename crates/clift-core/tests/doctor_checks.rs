//! Every `doctor` check, and every way each one can fail.
//!
//! The acceptance criterion is not "the checks exist" but "each one has a test
//! that makes it fail". A check whose failure branch has never run is a check
//! that has never been checked.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use clift_core::config::schema::SUPPORTED_VERSION;
use clift_core::diagnostics::{
    CheckName, ConfigState, DoctorReport, Environment, InjectionFacts, InjectionState, diagnose,
};
use clift_core::ports::{CheckStatus, TransportTarget};
use clift_core::testing::{
    CountingRandomness, FakeClipboard, FakeSshConfig, RecordingRelay, RecordingTransport,
};
use std::time::Duration;

/// Any live value; the relay checks in this file do not depend on it.
const TTL: Duration = Duration::from_secs(300);

fn healthy_config() -> ConfigState {
    ConfigState {
        exists: true,
        version: SUPPORTED_VERSION,
        supported: SUPPORTED_VERSION,
        warnings: Vec::new(),
        error: None,
    }
}

fn facts() -> clift_core::diagnostics::LocalFacts {
    clift_core::diagnostics::LocalFacts {
        platform: "aarch64-apple-darwin".to_string(),
        version: "0.1.0".to_string(),
    }
}

fn status_of(report: &DoctorReport, name: CheckName) -> CheckStatus {
    report
        .checks
        .iter()
        .find(|check| check.name == name)
        .unwrap_or_else(|| panic!("{name:?} was not reported"))
        .status
}

fn detail_of(report: &DoctorReport, name: CheckName) -> String {
    report
        .checks
        .iter()
        .find(|check| check.name == name)
        .unwrap_or_else(|| panic!("{name:?} was not reported"))
        .detail
        .clone()
}

fn target() -> TransportTarget {
    TransportTarget::new("core")
}

/// A transport whose probe reports everything as passing.
fn healthy_transport() -> RecordingTransport {
    let transport = RecordingTransport::new("/home/dev");
    for name in [
        "ssh client",
        "sftp client",
        "connection",
        "host key",
        "authentication",
        "sftp subsystem",
    ] {
        transport.report_check(name, CheckStatus::Pass, "fine");
    }
    transport
}

#[test]
fn all_thirteen_checks_are_reported_in_order_every_time() {
    let transport = healthy_transport();
    let ssh_config = FakeSshConfig::resolving("dev", "192.0.2.10", 2222);
    let environment = Environment {
        facts: facts(),
        ssh_config: &ssh_config,
        remote: &transport,
        upload: &transport,
        clipboard: None,
        remote_dir: None,
        injection: None,
        relay: None,
        config: healthy_config(),
    };

    for target in [Some(&target()), None] {
        let report = diagnose(&environment, target);
        let names: Vec<CheckName> = report.checks.iter().map(|check| check.name).collect();
        assert_eq!(
            names,
            CheckName::ALL.to_vec(),
            "with target = {}",
            target.is_some()
        );
    }
}

/// A check that fails must not stop the ones after it.
#[test]
fn one_failure_does_not_stop_the_remaining_checks() {
    let transport = RecordingTransport::new("/home/dev");
    transport.report_check("ssh client", CheckStatus::Fail, "ssh is not installed");
    for name in [
        "sftp client",
        "connection",
        "host key",
        "authentication",
        "sftp subsystem",
    ] {
        transport.report_check(name, CheckStatus::Pass, "fine");
    }
    let ssh_config = FakeSshConfig::resolving("dev", "192.0.2.10", 2222);
    let environment = Environment {
        facts: facts(),
        ssh_config: &ssh_config,
        remote: &transport,
        upload: &transport,
        clipboard: None,
        remote_dir: None,
        injection: None,
        relay: None,
        config: healthy_config(),
    };

    let report = diagnose(&environment, Some(&target()));
    assert_eq!(report.checks.len(), 13);
    assert_eq!(status_of(&report, CheckName::SshClient), CheckStatus::Fail);
    assert_eq!(
        status_of(&report, CheckName::UploadAndCleanup),
        CheckStatus::Pass,
        "the later checks still ran"
    );
    assert!(!report.passed());
    assert_eq!(report.failures(), 1);
}

/// Every failure carries exactly one command, and it is one the user can paste.
#[test]
fn every_failure_carries_one_runnable_command() {
    let transport = RecordingTransport::new("/home/dev");
    transport.report_check("ssh client", CheckStatus::Fail, "missing");
    transport.report_check("sftp client", CheckStatus::Fail, "missing");
    transport.report_check("connection", CheckStatus::Fail, "refused");
    transport.report_check("sftp subsystem", CheckStatus::Fail, "missing");
    transport.fail_ensure_dir("home is not writable");
    let ssh_config = FakeSshConfig::failing();
    let clipboard = FakeClipboard::failing();
    let mut broken = healthy_config();
    broken.error = Some("config.toml is not valid TOML".to_string());

    let environment = Environment {
        facts: facts(),
        ssh_config: &ssh_config,
        remote: &transport,
        upload: &transport,
        clipboard: Some(&clipboard),
        remote_dir: None,
        injection: None,
        relay: None,
        config: broken,
    };
    let report = diagnose(&environment, Some(&target()));

    let failures: Vec<&clift_core::diagnostics::DoctorCheck> = report
        .checks
        .iter()
        .filter(|check| check.status == CheckStatus::Fail)
        .collect();
    assert!(failures.len() >= 9, "expected many failures: {failures:?}");
    for failure in failures {
        let remedy = failure
            .remedy
            .as_ref()
            .unwrap_or_else(|| panic!("{:?} failed without a command", failure.name));
        assert!(
            !remedy.command().trim().is_empty(),
            "{:?} has an empty command",
            failure.name
        );
        assert!(
            !remedy.command().contains('\n'),
            "{:?} offers more than one command",
            failure.name
        );
    }
}

/// The Fail branch of each check that has one, exercised individually.
#[test]
fn each_check_that_can_fail_has_its_failure_exercised() {
    // clipboard
    let transport = healthy_transport();
    let ssh_config = FakeSshConfig::resolving("dev", "192.0.2.10", 2222);
    let clipboard = FakeClipboard::failing();
    let environment = Environment {
        facts: facts(),
        ssh_config: &ssh_config,
        remote: &transport,
        upload: &transport,
        clipboard: Some(&clipboard),
        remote_dir: None,
        injection: None,
        relay: None,
        config: healthy_config(),
    };
    let report = diagnose(&environment, Some(&target()));
    assert_eq!(status_of(&report, CheckName::Clipboard), CheckStatus::Fail);

    // ssh client, sftp client, authentication, sftp subsystem
    for (probe_name, check) in [
        ("ssh client", CheckName::SshClient),
        ("sftp client", CheckName::SftpClient),
        ("connection", CheckName::Authentication),
        ("host key", CheckName::Authentication),
        ("authentication", CheckName::Authentication),
        ("sftp subsystem", CheckName::SftpSubsystem),
    ] {
        let transport = RecordingTransport::new("/home/dev");
        transport.report_check(probe_name, CheckStatus::Fail, "broken");
        let environment = Environment {
            facts: facts(),
            ssh_config: &ssh_config,
            remote: &transport,
            upload: &transport,
            clipboard: None,
            remote_dir: None,
            injection: None,
            relay: None,
            config: healthy_config(),
        };
        let report = diagnose(&environment, Some(&target()));
        assert_eq!(
            status_of(&report, check),
            CheckStatus::Fail,
            "{probe_name} did not fail {check:?}"
        );
    }

    // host resolution
    let failing_config = FakeSshConfig::failing();
    let transport = healthy_transport();
    let environment = Environment {
        facts: facts(),
        ssh_config: &failing_config,
        remote: &transport,
        upload: &transport,
        clipboard: None,
        remote_dir: None,
        injection: None,
        relay: None,
        config: healthy_config(),
    };
    let report = diagnose(&environment, Some(&target()));
    assert_eq!(
        status_of(&report, CheckName::HostResolution),
        CheckStatus::Fail
    );

    // remote home, inbox permissions and upload all fail together when the
    // inbox cannot be prepared, and the last two say they were not checked.
    let transport = healthy_transport();
    transport.fail_ensure_dir("permission denied");
    let environment = Environment {
        facts: facts(),
        ssh_config: &ssh_config,
        remote: &transport,
        upload: &transport,
        clipboard: None,
        remote_dir: None,
        injection: None,
        relay: None,
        config: healthy_config(),
    };
    let report = diagnose(&environment, Some(&target()));
    for name in [
        CheckName::RemoteHome,
        CheckName::InboxPermissions,
        CheckName::UploadAndCleanup,
    ] {
        assert_eq!(status_of(&report, name), CheckStatus::Fail, "{name:?}");
    }
    assert!(detail_of(&report, CheckName::InboxPermissions).contains("not checked"));

    // upload and cleanup on its own
    let transport = healthy_transport();
    transport.fail_upload_of("/home/dev/.cache/clift/inbox/clift-selfcheck");
    let environment = Environment {
        facts: facts(),
        ssh_config: &ssh_config,
        remote: &transport,
        upload: &transport,
        clipboard: None,
        remote_dir: None,
        injection: None,
        relay: None,
        config: healthy_config(),
    };
    let report = diagnose(&environment, Some(&target()));
    assert_eq!(
        status_of(&report, CheckName::UploadAndCleanup),
        CheckStatus::Fail
    );
    assert_eq!(
        status_of(&report, CheckName::InboxPermissions),
        CheckStatus::Pass,
        "the inbox was fine; only the write failed"
    );

    // config version
    let mut broken = healthy_config();
    broken.error = Some("version 99 is newer than this build supports".to_string());
    let transport = healthy_transport();
    let environment = Environment {
        facts: facts(),
        ssh_config: &ssh_config,
        remote: &transport,
        upload: &transport,
        clipboard: None,
        remote_dir: None,
        injection: None,
        relay: None,
        config: broken,
    };
    let report = diagnose(&environment, Some(&target()));
    assert_eq!(
        status_of(&report, CheckName::ConfigVersion),
        CheckStatus::Fail
    );
}

/// A capability this build does not have warns and says so. It never passes:
/// nothing was examined.
#[test]
fn a_capability_that_is_not_built_in_warns_rather_than_passing() {
    let transport = healthy_transport();
    let ssh_config = FakeSshConfig::resolving("dev", "192.0.2.10", 2222);
    let environment = Environment {
        facts: facts(),
        ssh_config: &ssh_config,
        remote: &transport,
        upload: &transport,
        clipboard: None,
        remote_dir: None,
        injection: None,
        relay: None,
        config: healthy_config(),
    };
    let report = diagnose(&environment, Some(&target()));

    for name in [
        CheckName::Clipboard,
        CheckName::KeystrokeInjection,
        // A relay is optional -- Fast Mode is a complete way to use Clift -- so
        // its absence warns like the others rather than failing.
        CheckName::Relay,
    ] {
        assert_eq!(status_of(&report, name), CheckStatus::Warn, "{name:?}");
        assert!(
            detail_of(&report, name).contains("not checked"),
            "{name:?} must say it was not checked: {}",
            detail_of(&report, name)
        );
    }
    // Warnings are not failures: an installation with nothing configured for
    // Fast Mode is incomplete, not broken.
    assert!(report.passed());
    assert_eq!(report.warnings(), 3);
}

/// On a machine using Universal Mode, the eight host checks say that Fast Mode
/// is optional instead of telling the user to configure it.
///
/// They are still warnings, not skips. What must not happen is eight yellow
/// lines whose one runnable command sets up a mode the user deliberately is
/// not using: a report full of advice nobody should follow is a report people
/// learn to stop reading.
#[test]
fn with_a_relay_configured_the_host_checks_call_fast_mode_optional() {
    let transport = healthy_transport();
    let ssh_config = FakeSshConfig::resolving("dev", "192.0.2.10", 2222);
    let relay = RecordingRelay::new();
    let random = CountingRandomness::new();
    let settings =
        clift_core::universal::RelaySettings::new("https://relay.example.com", 8 * 1024, TTL)
            .unwrap_or_else(|error| panic!("{error}"));
    let environment = Environment {
        facts: facts(),
        ssh_config: &ssh_config,
        remote: &transport,
        upload: &transport,
        clipboard: None,
        remote_dir: None,
        injection: None,
        relay: Some(clift_core::diagnostics::RelayProbe {
            settings: &settings,
            relay: &relay,
            random: &random,
        }),
        config: healthy_config(),
    };
    let report = diagnose(&environment, None);

    for name in [
        CheckName::SshClient,
        CheckName::SftpClient,
        CheckName::HostResolution,
        CheckName::Authentication,
        CheckName::SftpSubsystem,
        CheckName::RemoteHome,
        CheckName::InboxPermissions,
        CheckName::UploadAndCleanup,
    ] {
        assert_eq!(status_of(&report, name), CheckStatus::Warn, "{name:?}");
        let detail = detail_of(&report, name);
        assert!(
            detail.contains("Universal Mode does not need one"),
            "{name:?} still reads as an unconfigured Fast Mode: {detail}"
        );
        let remedy = report
            .checks
            .iter()
            .find(|check| check.name == name)
            .and_then(|check| check.remedy.as_ref())
            .unwrap_or_else(|| panic!("{name:?} has no remedy"));
        assert!(
            remedy.description().contains("only if you want Fast Mode"),
            "{name:?}: {}",
            remedy.description()
        );
        assert_eq!(remedy.command(), "clift setup <ssh-host>", "{name:?}");
    }
}

/// Without a relay the same eight lines are the ordinary "set a host up"
/// advice, because then there is nothing else Clift could be doing.
#[test]
fn without_a_relay_the_host_checks_still_say_to_configure_one() {
    let transport = healthy_transport();
    let ssh_config = FakeSshConfig::resolving("dev", "192.0.2.10", 2222);
    let environment = Environment {
        facts: facts(),
        ssh_config: &ssh_config,
        remote: &transport,
        upload: &transport,
        clipboard: None,
        remote_dir: None,
        injection: None,
        relay: None,
        config: healthy_config(),
    };
    let report = diagnose(&environment, None);
    assert!(
        detail_of(&report, CheckName::SshClient).contains("no target is configured"),
        "{}",
        detail_of(&report, CheckName::SshClient)
    );
}

/// With nothing configured, the host checks say why rather than failing.
#[test]
fn without_a_configured_target_the_host_checks_explain_themselves() {
    let transport = healthy_transport();
    let ssh_config = FakeSshConfig::resolving("dev", "192.0.2.10", 2222);
    let mut none_yet = healthy_config();
    none_yet.exists = false;
    let environment = Environment {
        facts: facts(),
        ssh_config: &ssh_config,
        remote: &transport,
        upload: &transport,
        clipboard: None,
        remote_dir: None,
        injection: None,
        relay: None,
        config: none_yet,
    };
    let report = diagnose(&environment, None);

    assert_eq!(status_of(&report, CheckName::Platform), CheckStatus::Pass);
    assert_eq!(status_of(&report, CheckName::SshClient), CheckStatus::Warn);
    assert!(detail_of(&report, CheckName::SshClient).contains("no target is configured"));
    assert_eq!(
        status_of(&report, CheckName::ConfigVersion),
        CheckStatus::Warn
    );
    assert_eq!(
        transport.call_count(),
        0,
        "with no target there is nothing to ask a host"
    );
}

/// Everything the keystroke check can say, said for the right reason.
///
/// The check exists because of one failure that is invisible from either end:
/// macOS grants the permission to a binary, so a user who allows one copy of
/// `clift` and registers another to run at login has two halves that each look
/// right and do not work together. Pressing the key then does nothing at all,
/// with no error anywhere -- which is exactly the shape of problem a report is
/// for.
mod keystroke_injection {
    use super::{
        CheckName, CheckStatus, DoctorReport, Environment, FakeSshConfig, InjectionFacts,
        InjectionState, diagnose, facts, healthy_config, healthy_transport, status_of, target,
    };

    fn report_with(injection: Option<InjectionFacts>) -> DoctorReport {
        let transport = healthy_transport();
        let ssh_config = FakeSshConfig::resolving("dev", "192.0.2.10", 2222);
        let environment = Environment {
            facts: facts(),
            ssh_config: &ssh_config,
            remote: &transport,
            upload: &transport,
            clipboard: None,
            remote_dir: None,
            injection,
            relay: None,
            config: healthy_config(),
        };
        diagnose(&environment, Some(&target()))
    }

    fn ready(program: Option<&str>, helper: Option<&str>) -> InjectionFacts {
        InjectionFacts {
            state: InjectionState::Ready,
            program: program.map(str::to_string),
            helper: helper.map(str::to_string),
        }
    }

    fn line(report: &DoctorReport) -> &clift_core::diagnostics::DoctorCheck {
        report
            .checks
            .iter()
            .find(|check| check.name == CheckName::KeystrokeInjection)
            .expect("the check is always reported")
    }

    #[test]
    fn a_build_without_the_adapter_warns_rather_than_passing() {
        let report = report_with(None);
        let check = line(&report);
        assert_eq!(check.status, CheckStatus::Warn);
        assert!(
            check.detail.contains("not checked"),
            "a capability that was never examined must not read as examined: {}",
            check.detail
        );
    }

    #[test]
    fn a_platform_without_an_implementation_says_what_to_do_instead() {
        let report = report_with(Some(InjectionFacts {
            state: InjectionState::Unsupported {
                reason: "Wayland has no equivalent".to_string(),
            },
            program: None,
            helper: None,
        }));
        let check = line(&report);
        assert_eq!(check.status, CheckStatus::Warn);
        assert!(check.detail.contains("Wayland has no equivalent"));
        assert_eq!(
            check
                .remedy
                .as_ref()
                .map(clift_core::error::Remedy::command),
            Some("clift paste --copy"),
        );
    }

    /// A missing permission is a warning, not a failure: `--copy` is a complete
    /// way to use Clift, and calling that installation broken would teach the
    /// user to stop reading this report.
    #[test]
    fn a_missing_permission_warns_names_the_binary_and_hands_over_the_pane() {
        let report = report_with(Some(InjectionFacts {
            state: InjectionState::NeedsPermission {
                command: Some("open settings".to_string()),
            },
            program: Some("/usr/local/bin/clift".to_string()),
            helper: None,
        }));
        let check = line(&report);
        assert_eq!(check.status, CheckStatus::Warn);
        assert!(
            check.detail.contains("/usr/local/bin/clift"),
            "which binary is the question being asked: {}",
            check.detail
        );
        assert_eq!(
            check
                .remedy
                .as_ref()
                .map(clift_core::error::Remedy::command),
            Some("open settings"),
        );
        assert!(report.passed(), "a warning is not a failure");
    }

    /// No pane to open is not a reason to leave the user with nothing to run.
    #[test]
    fn a_missing_permission_with_nowhere_to_grant_it_still_offers_the_fall_back() {
        let report = report_with(Some(InjectionFacts {
            state: InjectionState::NeedsPermission { command: None },
            program: None,
            helper: None,
        }));
        assert_eq!(
            line(&report)
                .remedy
                .as_ref()
                .map(clift_core::error::Remedy::command),
            Some("clift paste --copy"),
        );
    }

    #[test]
    fn a_granted_permission_passes_and_names_what_it_can_do() {
        let report = report_with(Some(ready(Some("/usr/local/bin/clift"), None)));
        let check = line(&report);
        assert_eq!(check.status, CheckStatus::Pass);
        assert!(
            check.detail.contains("can type into the focused window"),
            "{}",
            check.detail
        );
        assert!(
            check.detail.contains("no hotkey helper"),
            "the helper's absence is a fact worth reporting: {}",
            check.detail
        );
        assert!(
            check.remedy.is_none(),
            "nothing is wrong, so nothing to run"
        );
    }

    #[test]
    fn a_helper_that_matches_the_running_binary_passes() {
        let report = report_with(Some(ready(
            Some("/usr/local/bin/clift"),
            Some("/usr/local/bin/clift"),
        )));
        assert_eq!(
            status_of(&report, CheckName::KeystrokeInjection),
            CheckStatus::Pass
        );
    }

    /// The one that cost an afternoon: permission granted, helper registered,
    /// and the key does nothing because they are two different files.
    #[test]
    fn a_helper_registered_against_another_binary_warns_and_says_which() {
        let report = report_with(Some(ready(
            Some("/usr/local/bin/clift"),
            Some("/Users/x/target/release/clift"),
        )));
        let check = line(&report);
        assert_eq!(check.status, CheckStatus::Warn);
        assert!(check.detail.contains("/Users/x/target/release/clift"));
        assert!(check.detail.contains("/usr/local/bin/clift"));
        assert_eq!(
            check
                .remedy
                .as_ref()
                .map(clift_core::error::Remedy::command),
            Some("clift hotkey --install"),
        );
    }
}
