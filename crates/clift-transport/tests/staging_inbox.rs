//! The remote inbox, against a real server.
//!
//! The unit tests in `clift-core` pin down where the inbox goes. These check
//! the two things only a real server can answer: that the directory really
//! comes out as 0700, and that an inbox which is already there with looser
//! permissions is reported rather than quietly tightened.

#![allow(clippy::unwrap_used)]

#[path = "../../../tests/e2e/fixtures.rs"]
mod fixtures;

use clift_core::ports::TransportTarget;
use clift_core::staging::{InboxRootSource, ensure_inbox, locate_inbox};
use clift_transport::probe::OpenSshTransport;
use clift_transport::proc::SshRunner;
use fixtures::{SshdFixture, Topology, skip_without_docker};

fn transport(fixture: &SshdFixture) -> OpenSshTransport {
    OpenSshTransport::with_runner(SshRunner::new().with_config_file(fixture.ssh_config()))
}

fn remote_mode(fixture: &SshdFixture, path: &str) -> String {
    let output = fixture.ssh(&format!("stat -c %a \"{path}\""));
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

#[test]
fn the_inbox_is_created_private_under_the_home_directory() {
    if skip_without_docker("the_inbox_is_created_private_under_the_home_directory") {
        return;
    }
    let fixture = SshdFixture::start(Topology::Normal);
    let location = ensure_inbox(
        &transport(&fixture),
        &TransportTarget::new(fixture.alias()),
        None,
    )
    .unwrap();

    assert_eq!(
        location.root().as_str(),
        format!("{}/.cache/clift/inbox", fixture.remote_home())
    );
    assert_eq!(*location.source(), InboxRootSource::HomeDefault);
    assert_eq!(location.warning(), None);
    assert_eq!(remote_mode(&fixture, location.root().as_str()), "700");
    assert!(
        !location.root().as_str().contains("/tmp"),
        "the default root must never be a public temporary directory"
    );
}

#[test]
fn an_inbox_that_already_exists_with_looser_permissions_is_refused_not_fixed() {
    if skip_without_docker(
        "an_inbox_that_already_exists_with_looser_permissions_is_refused_not_fixed",
    ) {
        return;
    }
    let fixture = SshdFixture::start(Topology::Normal);
    assert!(
        fixture
            .ssh("mkdir -p \"$HOME/.cache/clift/inbox\" && chmod 755 \"$HOME/.cache/clift/inbox\"")
            .status
            .success()
    );

    let error = ensure_inbox(
        &transport(&fixture),
        &TransportTarget::new(fixture.alias()),
        None,
    )
    .expect_err("a world readable inbox must not be accepted");

    assert_eq!(error.exit_code().as_u8(), 25);
    assert!(error.message().contains("0755"), "{error}");
    assert!(error.remedy().is_some(), "the user needs a way forward");
    assert_eq!(
        remote_mode(
            &fixture,
            &format!("{}/.cache/clift/inbox", fixture.remote_home())
        ),
        "755",
        "Clift must not have changed the permissions it objected to"
    );
}

#[test]
fn a_home_that_cannot_be_written_to_fails_with_a_remote_directory_error() {
    if skip_without_docker("a_home_that_cannot_be_written_to_fails_with_a_remote_directory_error") {
        return;
    }
    let fixture = SshdFixture::start(Topology::ReadonlyHome);
    let error = ensure_inbox(
        &transport(&fixture),
        &TransportTarget::new(fixture.alias()),
        None,
    )
    .expect_err("a read-only home cannot hold an inbox");

    // Exit code 25 and a remedy, never a silent move somewhere writable.
    assert_eq!(error.exit_code().as_u8(), 25);
    assert!(
        error.message().to_lowercase().contains("permission denied"),
        "the server's own reason must survive: {error}"
    );
    let remedy = error.remedy().expect("a failure must say what to do");
    assert!(
        remedy.command().starts_with("ssh "),
        "the fix must be runnable as written: {remedy:?}"
    );
}

#[test]
fn a_cache_home_the_host_advertises_is_used() {
    if skip_without_docker("a_cache_home_the_host_advertises_is_used") {
        return;
    }
    let fixture = SshdFixture::start(Topology::Normal);
    let advertised = format!("{}/somewhere else", fixture.remote_home());
    assert!(
        fixture
            .ssh(&format!(
                "printf 'XDG_CACHE_HOME={advertised}\\n' > \"$HOME/.ssh/environment\" && \
                 chmod 600 \"$HOME/.ssh/environment\""
            ))
            .status
            .success()
    );

    let location = locate_inbox(
        &transport(&fixture),
        &TransportTarget::new(fixture.alias()),
        None,
    )
    .unwrap();
    assert_eq!(*location.source(), InboxRootSource::CacheHome);
    assert_eq!(
        location.root().as_str(),
        format!("{advertised}/clift/inbox")
    );
}

#[test]
fn a_cache_home_in_a_public_temporary_directory_is_refused_by_a_real_host_too() {
    if skip_without_docker(
        "a_cache_home_in_a_public_temporary_directory_is_refused_by_a_real_host_too",
    ) {
        return;
    }
    let fixture = SshdFixture::start(Topology::Normal);
    assert!(
        fixture
            .ssh(
                "printf 'XDG_CACHE_HOME=/tmp/shared\\n' > \"$HOME/.ssh/environment\" && \
                 chmod 600 \"$HOME/.ssh/environment\""
            )
            .status
            .success()
    );

    let location = locate_inbox(
        &transport(&fixture),
        &TransportTarget::new(fixture.alias()),
        None,
    )
    .unwrap();
    assert_eq!(
        location.root().as_str(),
        format!("{}/.cache/clift/inbox", fixture.remote_home()),
        "a world writable cache home must not be used"
    );
    let warning = location
        .warning()
        .expect("the user must be told it was ignored");
    assert!(warning.contains("XDG_CACHE_HOME"), "{warning}");
}
