//! Cleanup, from the attacker's and the unlucky user's side.
//!
//! The boundary itself is covered by `crates/clift-transport/tests/staging_clean.rs`.
//! What is here is the case that only concurrency can produce: a sweep running
//! while a send is still writing, which must not remove the batch being written.
//!
//! The uninstall half of this suite waits for terminal integration to exist;
//! it is tracked as follow-up work.

#![allow(clippy::unwrap_used, clippy::expect_used)]

#[path = "../e2e/fixtures.rs"]
mod fixtures;

use clift_core::domain::{FileKind, LocalAttachment, SafeFileName};
use clift_core::error::{CliftError, ErrorKind, Stage};
use clift_core::ports::{Clock, IdSource, TransportTarget};
use clift_core::staging::{Action, Retention, clean, ensure_inbox};
use clift_core::usecase::{SendPolicy, perform};
use clift_transport::probe::OpenSshTransport;
use clift_transport::proc::SshRunner;
use fixtures::{SshdFixture, Topology, skip_without_docker};
use std::path::PathBuf;
use std::time::{Duration, SystemTime};

struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> SystemTime {
        SystemTime::now()
    }
}

struct SystemIdSource;

impl IdSource for SystemIdSource {
    fn new_batch_id(&self) -> Result<clift_core::domain::BatchId, CliftError> {
        let mut bytes = [0u8; clift_core::domain::BATCH_ID_BYTES];
        getrandom::fill(&mut bytes).map_err(|error| {
            CliftError::new(
                Stage::Staging,
                ErrorKind::Internal,
                "the operating system random source is unavailable",
            )
            .with_source(error)
        })?;
        clift_core::domain::BatchId::from_random_bytes(bytes)
            .map_err(|error| error.into_clift(Stage::Staging, ErrorKind::Internal))
    }
}

fn transport(fixture: &SshdFixture) -> OpenSshTransport {
    OpenSshTransport::with_runner(SshRunner::new().with_config_file(fixture.ssh_config()))
}

fn attachment(fixture: &SshdFixture, name: &str, bytes: usize) -> LocalAttachment {
    let path: PathBuf = fixture.workdir().join(name);
    std::fs::write(&path, vec![b'x'; bytes]).unwrap();
    let size = std::fs::metadata(&path).unwrap().len();
    LocalAttachment::new(path, SafeFileName::sanitize(name), size, FileKind::Regular).unwrap()
}

/// A sweep that runs while a send is in flight must not take the batch the send
/// is writing into.
///
/// The retention is what protects it: a batch created seconds ago is not
/// expired, whatever else is happening. This is the assertion, because the
/// alternative -- a lock -- is not something Clift can have on a host it does
/// not run code on.
#[test]
fn a_sweep_running_during_a_send_does_not_take_the_batch_being_written() {
    if skip_without_docker("a_sweep_running_during_a_send_does_not_take_the_batch_being_written") {
        return;
    }
    let fixture = SshdFixture::start(Topology::Normal);
    let transport = transport(&fixture);
    let target = TransportTarget::new(fixture.alias());
    let inbox = ensure_inbox(&transport, &target, None).unwrap();

    // Something old enough to be swept, so the sweep has real work to do.
    let old = format!("{}/2026-08-01/aaaa", inbox.root());
    assert!(
        fixture
            .ssh(&format!(
                "mkdir -p '{old}' && printf old > '{old}/a.png' && \
                 touch -d '2020-01-01 00:00' '{old}/a.png' '{old}'"
            ))
            .status
            .success()
    );

    // A send large enough to still be running when the sweep starts.
    let payload = attachment(&fixture, "large.bin", 8 * 1024 * 1024);
    let policy = SendPolicy {
        retention: Some(Duration::from_secs(24 * 60 * 60)),
        ..SendPolicy::default()
    };

    let (outcome, report) = std::thread::scope(|scope| {
        let sending = scope.spawn(|| {
            perform(
                &transport,
                &target,
                std::slice::from_ref(&payload),
                &policy,
                &SystemClock,
                &SystemIdSource,
            )
        });
        // Sweeps repeatedly while the send is in flight, so at least one of
        // them overlaps the upload.
        let mut last = None;
        while !sending.is_finished() {
            last = Some(
                clean(
                    &transport,
                    &target,
                    inbox.root(),
                    Retention::OlderThan(Duration::from_secs(24 * 60 * 60)),
                    Action::Remove,
                    SystemTime::now(),
                )
                .unwrap(),
            );
        }
        (sending.join().unwrap(), last)
    });

    let outcome = outcome.expect("the send must survive a concurrent sweep");
    assert_eq!(outcome.batch().files().len(), 1);

    // The file the send produced is still there, whole.
    let path = outcome.batch().files()[0].path().as_str().to_string();
    let size = String::from_utf8_lossy(&fixture.ssh(&format!("stat -c %s '{path}'")).stdout)
        .trim()
        .to_string();
    assert_eq!(size, (8 * 1024 * 1024).to_string(), "{path}");

    // And the old batch did get swept, so the concurrent run was doing
    // something rather than finding nothing to do.
    assert!(
        report.is_some_and(|report| report.batches > 0)
            || String::from_utf8_lossy(
                &fixture.ssh(&format!("test -d '{old}' || echo gone")).stdout
            )
            .contains("gone"),
        "the sweep never removed the expired batch, so it proved nothing"
    );
}

/// Running cleanup twice removes the same things once and reports nothing odd
/// the second time.
#[test]
fn cleaning_twice_is_the_same_as_cleaning_once() {
    if skip_without_docker("cleaning_twice_is_the_same_as_cleaning_once") {
        return;
    }
    let fixture = SshdFixture::start(Topology::Normal);
    let transport = transport(&fixture);
    let target = TransportTarget::new(fixture.alias());
    let inbox = ensure_inbox(&transport, &target, None).unwrap();

    let batch = format!("{}/2026-08-01/aaaa", inbox.root());
    assert!(
        fixture
            .ssh(&format!("mkdir -p '{batch}' && printf x > '{batch}/a.png'"))
            .status
            .success()
    );

    let first = clean(
        &transport,
        &target,
        inbox.root(),
        Retention::Everything,
        Action::Remove,
        SystemTime::now(),
    )
    .unwrap();
    assert_eq!(first.batches, 1);

    let second = clean(
        &transport,
        &target,
        inbox.root(),
        Retention::Everything,
        Action::Remove,
        SystemTime::now(),
    )
    .unwrap();
    assert_eq!(second.batches, 0);
    assert!(second.skipped.is_empty(), "{:?}", second.skipped);
}
