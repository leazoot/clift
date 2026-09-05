//! Probing a real server, including servers that are broken in a
//! specific way.
//!
//! The point of these tests is attribution. Any probe can say "it did not
//! work"; what matters is that a missing SFTP subsystem does not read as a
//! connection failure, and that a rejected key does not read as a bad host key.

#![allow(clippy::unwrap_used)]

#[path = "../../../tests/e2e/fixtures.rs"]
mod fixtures;

use clift_core::ports::{CheckStatus, ProbeReport, TransportTarget};
use clift_transport::probe::{
    CHECK_AUTHENTICATION, CHECK_CONNECTION, CHECK_HOST_KEY, CHECK_SFTP_CLIENT,
    CHECK_SFTP_SUBSYSTEM, CHECK_SSH_CLIENT, OpenSshTransport,
};
use clift_transport::proc::SshRunner;
use fixtures::{SshdFixture, Topology, skip_without_docker};
use std::path::Path;

fn transport(config: &Path) -> OpenSshTransport {
    OpenSshTransport::with_runner(SshRunner::new().with_config_file(config))
}

fn status_of<'a>(report: &'a ProbeReport, name: &str) -> &'a str {
    let check = report
        .checks
        .iter()
        .find(|check| check.name == name)
        .unwrap_or_else(|| panic!("the report has no {name} check: {report:?}"));
    match check.status {
        CheckStatus::Pass => "pass",
        CheckStatus::Warn => "warn",
        CheckStatus::Fail => "fail",
    }
}

fn detail_of<'a>(report: &'a ProbeReport, name: &str) -> &'a str {
    report
        .checks
        .iter()
        .find(|check| check.name == name)
        .map(|check| check.detail.as_str())
        .unwrap_or_else(|| panic!("the report has no {name} check: {report:?}"))
}

/// The specification: no failure may ever propose turning verification off.
fn assert_no_bypass_advice(report: &ProbeReport) {
    for check in &report.checks {
        let text = check.detail.to_lowercase();
        for forbidden in [
            "stricthostkeychecking",
            "userknownhostsfile=/dev/null",
            "-o ",
        ] {
            assert!(
                !text.contains(forbidden),
                "a check suggested weakening verification: {check:?}"
            );
        }
    }
}

#[test]
fn a_healthy_host_passes_every_check() {
    if skip_without_docker("a_healthy_host_passes_every_check") {
        return;
    }
    let fixture = SshdFixture::start(Topology::Normal);
    let report = transport(fixture.ssh_config())
        .probe(&TransportTarget::new(fixture.alias()))
        .unwrap();

    for name in [
        CHECK_SSH_CLIENT,
        CHECK_SFTP_CLIENT,
        CHECK_CONNECTION,
        CHECK_HOST_KEY,
        CHECK_AUTHENTICATION,
        CHECK_SFTP_SUBSYSTEM,
    ] {
        assert_eq!(status_of(&report, name), "pass", "{name}: {report:?}");
    }
    assert!(report.passed());
    assert_no_bypass_advice(&report);
}

#[test]
fn a_missing_sftp_subsystem_is_named_as_such_not_as_a_connection_failure() {
    if skip_without_docker("a_missing_sftp_subsystem_is_named_as_such_not_as_a_connection_failure")
    {
        return;
    }
    let fixture = SshdFixture::start(Topology::NoSftp);
    let report = transport(fixture.ssh_config())
        .probe(&TransportTarget::new(fixture.alias()))
        .unwrap();

    assert_eq!(status_of(&report, CHECK_CONNECTION), "pass");
    assert_eq!(status_of(&report, CHECK_HOST_KEY), "pass");
    assert_eq!(status_of(&report, CHECK_AUTHENTICATION), "pass");
    assert_eq!(status_of(&report, CHECK_SFTP_SUBSYSTEM), "fail");
    assert!(!report.passed());

    let detail = detail_of(&report, CHECK_SFTP_SUBSYSTEM);
    assert!(
        detail.contains("does not offer the SFTP subsystem"),
        "the failure must name the subsystem: {detail}"
    );
    assert!(
        detail.contains("subsystem request failed"),
        "OpenSSH's own words must survive: {detail}"
    );
    assert_no_bypass_advice(&report);
}

#[test]
fn a_rejected_key_fails_authentication_and_keeps_opensshs_reason() {
    if skip_without_docker("a_rejected_key_fails_authentication_and_keeps_opensshs_reason") {
        return;
    }
    let fixture = SshdFixture::start(Topology::Normal);
    let spare = fixture.spare_identity();
    let config = fixture.variant_config("wrongkey", |original| {
        original.replace(
            fixture.workdir().join("id_ed25519").to_str().unwrap(),
            spare.to_str().unwrap(),
        )
    });

    let report = transport(&config)
        .probe(&TransportTarget::new(fixture.alias()))
        .unwrap();

    assert_eq!(status_of(&report, CHECK_CONNECTION), "pass");
    assert_eq!(status_of(&report, CHECK_HOST_KEY), "pass");
    assert_eq!(status_of(&report, CHECK_AUTHENTICATION), "fail");
    assert!(
        detail_of(&report, CHECK_AUTHENTICATION).contains("Permission denied"),
        "the summary must keep OpenSSH's reason: {report:?}"
    );

    // Nothing further is attempted once a check fails: no second connection,
    // no fallback with different settings.
    assert_eq!(status_of(&report, CHECK_SFTP_SUBSYSTEM), "warn");
    assert!(detail_of(&report, CHECK_SFTP_SUBSYSTEM).starts_with("not checked:"));
    assert_no_bypass_advice(&report);
}

#[test]
fn an_unknown_host_key_fails_the_host_key_check_before_authentication() {
    if skip_without_docker("an_unknown_host_key_fails_the_host_key_check_before_authentication") {
        return;
    }
    let fixture = SshdFixture::start(Topology::Normal);
    let empty = fixture.workdir().join("known_hosts.empty");
    std::fs::write(&empty, "").unwrap();
    let config = fixture.variant_config("unknownkey", |original| {
        original.replace(
            fixture.workdir().join("known_hosts").to_str().unwrap(),
            empty.to_str().unwrap(),
        )
    });

    let report = transport(&config)
        .probe(&TransportTarget::new(fixture.alias()))
        .unwrap();

    assert_eq!(status_of(&report, CHECK_HOST_KEY), "fail");
    assert!(
        detail_of(&report, CHECK_HOST_KEY).contains("not in known_hosts"),
        "{report:?}"
    );
    assert_eq!(status_of(&report, CHECK_AUTHENTICATION), "warn");
    assert_no_bypass_advice(&report);
}

#[test]
fn a_changed_host_key_is_reported_verbatim_and_never_offered_a_bypass() {
    if skip_without_docker("a_changed_host_key_is_reported_verbatim_and_never_offered_a_bypass") {
        return;
    }
    let fixture = SshdFixture::start(Topology::Normal);
    let mismatched = fixture.mismatched_known_hosts();
    let config = fixture.variant_config("changedkey", |original| {
        original.replace(
            fixture.workdir().join("known_hosts").to_str().unwrap(),
            mismatched.to_str().unwrap(),
        )
    });

    let report = transport(&config)
        .probe(&TransportTarget::new(fixture.alias()))
        .unwrap();

    assert_eq!(status_of(&report, CHECK_HOST_KEY), "fail");
    let detail = detail_of(&report, CHECK_HOST_KEY);
    assert!(
        detail.contains("has changed"),
        "a changed key must not be reported as merely unknown: {detail}"
    );
    assert!(
        detail.contains("REMOTE HOST IDENTIFICATION HAS CHANGED"),
        "OpenSSH's warning must be passed through untouched: {detail}"
    );
    assert_eq!(status_of(&report, CHECK_AUTHENTICATION), "warn");
    assert_no_bypass_advice(&report);
}
