//! The cleanup boundary, against a real server.
//!
//! The unit tests decide what the rule is. These check the part only a real
//! filesystem can answer: that a symbolic link planted in the inbox is neither
//! followed nor removed, and that a directory beside the inbox with a similar
//! name is not touched at all.

#![allow(clippy::unwrap_used, clippy::expect_used)]

#[path = "../../../tests/e2e/fixtures.rs"]
mod fixtures;

use clift_core::domain::RemotePath;
use clift_core::ports::TransportTarget;
use clift_core::staging::{Action, Retention, clean, ensure_inbox};
use clift_transport::probe::OpenSshTransport;
use clift_transport::proc::SshRunner;
use fixtures::{SshdFixture, Topology, skip_without_docker};
use std::time::{Duration, SystemTime};

fn transport(fixture: &SshdFixture) -> OpenSshTransport {
    OpenSshTransport::with_runner(SshRunner::new().with_config_file(fixture.ssh_config()))
}

fn shell(fixture: &SshdFixture, command: &str) -> String {
    let output = fixture.ssh(command);
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

/// Two batches, one of them from long ago.
fn seed(fixture: &SshdFixture, inbox: &RemotePath) {
    let old = format!("{inbox}/2026-08-01/aaaa");
    let new = format!("{inbox}/2026-08-30/bbbb");
    assert!(
        fixture
            .ssh(&format!(
                "mkdir -p '{old}' '{new}' && \
                 printf old > '{old}/shot.png' && printf new > '{new}/shot.png' && \
                 touch -d '2020-01-01 00:00' '{old}' '{old}/shot.png'"
            ))
            .status
            .success()
    );
}

#[test]
fn a_symlink_out_of_the_inbox_is_neither_followed_nor_removed() {
    if skip_without_docker("a_symlink_out_of_the_inbox_is_neither_followed_nor_removed") {
        return;
    }
    let fixture = SshdFixture::start(Topology::Normal);
    let transport = transport(&fixture);
    let target = TransportTarget::new(fixture.alias());
    let inbox = ensure_inbox(&transport, &target, None).unwrap();
    seed(&fixture, inbox.root());

    // Planted by someone with access to the account: a link out of the inbox,
    // sitting exactly where a batch directory would be.
    let link = format!("{}/2026-08-01/etcetera", inbox.root());
    assert!(
        fixture
            .ssh(&format!("ln -s /etc '{link}'"))
            .status
            .success()
    );

    let report = clean(
        &transport,
        &target,
        inbox.root(),
        Retention::Everything,
        Action::Remove,
        SystemTime::now(),
    )
    .unwrap();

    // The link is still there, and so is what it points at.
    assert_eq!(
        shell(&fixture, &format!("test -L '{link}' && echo yes")),
        "yes"
    );
    assert_eq!(shell(&fixture, "test -f /etc/passwd && echo yes"), "yes");
    assert!(
        report
            .skipped
            .iter()
            .any(|entry| entry.contains("etcetera") && entry.contains("symbolic link")),
        "the link must be reported as skipped: {:?}",
        report.skipped
    );
}

/// A directory beside the inbox whose name merely starts the same way.
#[test]
fn a_neighbour_of_the_inbox_is_left_alone() {
    if skip_without_docker("a_neighbour_of_the_inbox_is_left_alone") {
        return;
    }
    let fixture = SshdFixture::start(Topology::Normal);
    let transport = transport(&fixture);
    let target = TransportTarget::new(fixture.alias());
    let inbox = ensure_inbox(&transport, &target, None).unwrap();
    seed(&fixture, inbox.root());

    let neighbour = format!("{}-old", inbox.root());
    assert!(
        fixture
            .ssh(&format!(
                "mkdir -p '{neighbour}/2026-08-01/cccc' && printf keep > '{neighbour}/2026-08-01/cccc/file.txt'"
            ))
            .status
            .success()
    );

    clean(
        &transport,
        &target,
        inbox.root(),
        Retention::Everything,
        Action::Remove,
        SystemTime::now(),
    )
    .unwrap();

    assert_eq!(
        shell(
            &fixture,
            &format!("cat '{neighbour}/2026-08-01/cccc/file.txt'")
        ),
        "keep",
        "a directory beside the inbox was touched"
    );
    assert_eq!(
        shell(
            &fixture,
            &format!("find '{}' -type f | wc -l", inbox.root())
        ),
        "0",
        "everything inside the inbox should be gone"
    );
}

/// Retention selects by modification time, and leaves the rest.
#[test]
fn only_the_batches_older_than_the_retention_are_removed() {
    if skip_without_docker("only_the_batches_older_than_the_retention_are_removed") {
        return;
    }
    let fixture = SshdFixture::start(Topology::Normal);
    let transport = transport(&fixture);
    let target = TransportTarget::new(fixture.alias());
    let inbox = ensure_inbox(&transport, &target, None).unwrap();
    seed(&fixture, inbox.root());

    let report = clean(
        &transport,
        &target,
        inbox.root(),
        Retention::OlderThan(Duration::from_secs(24 * 60 * 60)),
        Action::Remove,
        SystemTime::now(),
    )
    .unwrap();

    assert_eq!(report.batches, 1, "only the old batch: {report:?}");
    assert_eq!(report.files, 1);
    assert_eq!(report.bytes, 3, "the old file was three bytes");
    assert_eq!(
        shell(
            &fixture,
            &format!("cat '{}/2026-08-30/bbbb/shot.png'", inbox.root())
        ),
        "new",
        "today's batch must survive"
    );
    assert_eq!(
        shell(
            &fixture,
            &format!(
                "test -d '{}/2026-08-01' && echo yes || echo no",
                inbox.root()
            )
        ),
        "no",
        "an emptied date directory goes with its batches"
    );
}

/// Cleaning an inbox that is already empty is a no-op, not a failure.
#[test]
fn cleaning_an_empty_inbox_removes_nothing_and_reports_nothing() {
    if skip_without_docker("cleaning_an_empty_inbox_removes_nothing_and_reports_nothing") {
        return;
    }
    let fixture = SshdFixture::start(Topology::Normal);
    let transport = transport(&fixture);
    let target = TransportTarget::new(fixture.alias());
    let inbox = ensure_inbox(&transport, &target, None).unwrap();

    let report = clean(
        &transport,
        &target,
        inbox.root(),
        Retention::Everything,
        Action::Remove,
        SystemTime::now(),
    )
    .unwrap();
    assert_eq!(report.batches, 0);
    assert!(report.skipped.is_empty(), "{:?}", report.skipped);
}
