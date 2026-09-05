//! Batch directories on a real server.
//!
//! The unit tests in `clift-core` decide where a batch goes. This checks the
//! part only a server can answer: that both directories come out private, and
//! that two batches running at the same moment cannot land on each other.

#![allow(clippy::unwrap_used, clippy::expect_used)]

#[path = "../../../tests/e2e/fixtures.rs"]
mod fixtures;

use clift_core::domain::{BATCH_ID_BYTES, BatchId};
use clift_core::error::{CliftError, ErrorKind, Stage};
use clift_core::ports::{Clock, IdSource, TransportTarget};
use clift_core::staging::{create_batch, ensure_inbox, plan_batch};
use clift_transport::probe::OpenSshTransport;
use clift_transport::proc::SshRunner;
use fixtures::{SshdFixture, Topology, skip_without_docker};
use std::time::SystemTime;

/// The real thing, not a fake: the specification forbids test doubles on an integration
/// path. The production wiring of these two lives in the composition root and
/// arrives with the first command that needs it.
struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> SystemTime {
        SystemTime::now()
    }
}

struct SystemIdSource;

impl IdSource for SystemIdSource {
    fn new_batch_id(&self) -> Result<BatchId, CliftError> {
        let mut bytes = [0u8; BATCH_ID_BYTES];
        getrandom::fill(&mut bytes).map_err(|error| {
            CliftError::new(
                Stage::Staging,
                ErrorKind::Internal,
                "the operating system random source is unavailable",
            )
            .with_source(error)
        })?;
        BatchId::from_random_bytes(bytes)
            .map_err(|error| error.into_clift(Stage::Staging, ErrorKind::Internal))
    }
}

fn transport(fixture: &SshdFixture) -> OpenSshTransport {
    OpenSshTransport::with_runner(SshRunner::new().with_config_file(fixture.ssh_config()))
}

fn remote_mode(fixture: &SshdFixture, path: &str) -> String {
    let output = fixture.ssh(&format!("stat -c %a \"{path}\""));
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

#[test]
fn a_batch_directory_and_the_date_directory_above_it_are_both_private() {
    if skip_without_docker("a_batch_directory_and_the_date_directory_above_it_are_both_private") {
        return;
    }
    let fixture = SshdFixture::start(Topology::Normal);
    let transport = transport(&fixture);
    let target = TransportTarget::new(fixture.alias());

    let inbox = ensure_inbox(&transport, &target, None).unwrap();
    let plan = plan_batch(&inbox, &SystemClock, &SystemIdSource).unwrap();
    create_batch(&transport, &target, &plan).unwrap();

    assert_eq!(remote_mode(&fixture, plan.directory().as_str()), "700");
    let date_directory = format!("{}/{}", inbox.root(), plan.date());
    assert_eq!(
        remote_mode(&fixture, &date_directory),
        "700",
        "the date directory is created on the way and must be private too"
    );
    assert!(plan.directory().is_within(inbox.root()));
}

#[test]
fn two_batches_created_together_do_not_collide() {
    if skip_without_docker("two_batches_created_together_do_not_collide") {
        return;
    }
    let fixture = SshdFixture::start(Topology::Normal);
    let transport = transport(&fixture);
    let target = TransportTarget::new(fixture.alias());
    let inbox = ensure_inbox(&transport, &target, None).unwrap();

    let first = plan_batch(&inbox, &SystemClock, &SystemIdSource).unwrap();
    let second = plan_batch(&inbox, &SystemClock, &SystemIdSource).unwrap();
    assert_ne!(first.directory(), second.directory());

    create_batch(&transport, &target, &first).unwrap();
    create_batch(&transport, &target, &second).unwrap();

    // Same name in both, and neither overwrote the other: the batch directory
    // is what keeps them apart.
    for (plan, content) in [(&first, "one"), (&second, "two")] {
        let file = format!("{}/shot.png", plan.directory());
        assert!(
            fixture
                .ssh(&format!("printf '{content}' > \"{file}\""))
                .status
                .success()
        );
    }
    for (plan, content) in [(&first, "one"), (&second, "two")] {
        let file = format!("{}/shot.png", plan.directory());
        let read = fixture.ssh(&format!("cat \"{file}\""));
        assert_eq!(String::from_utf8_lossy(&read.stdout), content);
    }
}

#[test]
fn a_batch_identifier_is_not_derived_from_anything_predictable() {
    if skip_without_docker("a_batch_identifier_is_not_derived_from_anything_predictable") {
        return;
    }
    let fixture = SshdFixture::start(Topology::Normal);
    let transport = transport(&fixture);
    let target = TransportTarget::new(fixture.alias());
    let inbox = ensure_inbox(&transport, &target, None).unwrap();

    let plan = plan_batch(&inbox, &SystemClock, &SystemIdSource).unwrap();
    let id = plan.id().as_str();
    let now = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_secs();

    assert!(
        !id.contains(&now.to_string()),
        "the identifier embeds the clock"
    );
    assert!(
        !id.contains(&std::process::id().to_string()),
        "the identifier embeds the process id"
    );
    assert_eq!(id.len(), BATCH_ID_BYTES * 2);
}
