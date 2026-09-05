//! The limit check really does run before anything reaches the host.
//!
//! The unit tests pin the boundaries to the byte. What only a server can show
//! is the claim that matters operationally: an oversized batch leaves no batch
//! directory and no `.part` file behind, because it never got that far.

#![allow(clippy::unwrap_used, clippy::expect_used)]

#[path = "../../../tests/e2e/fixtures.rs"]
mod fixtures;

use clift_core::domain::{
    BATCH_ID_BYTES, BatchId, FileKind, Limits, LocalAttachment, SafeFileName,
};
use clift_core::error::{CliftError, ErrorKind, Stage};
use clift_core::ports::{Clock, IdSource, TransportTarget};
use clift_core::staging::{InboxLocation, ensure_inbox, plan_batch};
use clift_core::usecase::stage_attachments;
use clift_transport::probe::OpenSshTransport;
use clift_transport::proc::SshRunner;
use fixtures::{SshdFixture, Topology, skip_without_docker};
use std::path::PathBuf;
use std::time::SystemTime;

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

/// A real local file of the given size, sparse so the test stays fast, and an
/// attachment whose size is read back from the filesystem rather than asserted.
fn sparse_attachment(fixture: &SshdFixture, name: &str, bytes: u64) -> LocalAttachment {
    let path: PathBuf = fixture.workdir().join(name);
    let file = std::fs::File::create(&path).expect("the local file must be creatable");
    file.set_len(bytes).expect("sparse allocation");
    drop(file);

    let size = std::fs::metadata(&path).expect("the file exists").len();
    assert_eq!(size, bytes, "the local file is the size the test asked for");
    LocalAttachment::new(path, SafeFileName::sanitize(name), size, FileKind::Regular)
        .expect("a regular file at an absolute path")
}

/// Everything below the inbox root, one path per line.
fn everything_under(fixture: &SshdFixture, inbox: &InboxLocation) -> Vec<String> {
    let output = fixture.ssh(&format!("find \"{}\" -mindepth 1", inbox.root()));
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::to_string)
        .filter(|line| !line.is_empty())
        .collect()
}

#[test]
fn an_oversized_batch_leaves_no_directory_and_no_part_file() {
    if skip_without_docker("an_oversized_batch_leaves_no_directory_and_no_part_file") {
        return;
    }
    let fixture = SshdFixture::start(Topology::Normal);
    let transport = transport(&fixture);
    let target = TransportTarget::new(fixture.alias());
    let inbox = ensure_inbox(&transport, &target, None).unwrap();

    // One byte over the per-file ceiling, and a real file of that size.
    let oversized = sparse_attachment(&fixture, "huge.bin", 50 * 1024 * 1024 + 1);
    let plan = plan_batch(&inbox, &SystemClock, &SystemIdSource).unwrap();

    let error = stage_attachments(&transport, &target, &plan, Limits::default(), &[oversized])
        .expect_err("one byte over the per-file limit");

    assert_eq!(
        error.exit_code().as_u8(),
        26,
        "an over-limit batch is exit code 26: {error}"
    );
    assert!(error.to_string().contains("huge.bin"), "{error}");
    assert!(error.to_string().contains("50 MiB"), "{error}");

    assert!(
        everything_under(&fixture, &inbox).is_empty(),
        "nothing may exist under the inbox: {:?}",
        everything_under(&fixture, &inbox)
    );

    // The control: the same call with a batch inside the limits does create
    // the directory and the file, so the assertion above is testing the limit
    // check and not a broken fixture.
    let allowed = sparse_attachment(&fixture, "small.bin", 1024);
    let second = plan_batch(&inbox, &SystemClock, &SystemIdSource).unwrap();
    let staged =
        stage_attachments(&transport, &target, &second, Limits::default(), &[allowed]).unwrap();

    let landed = everything_under(&fixture, &inbox);
    assert!(
        landed.contains(&staged.files()[0].path().as_str().to_string()),
        "the allowed batch should have landed: {landed:?}"
    );
    assert!(
        !landed.iter().any(|path| path.ends_with(".part")),
        "a successful batch leaves no intermediate file: {landed:?}"
    );
}

#[test]
fn a_configured_ceiling_changes_what_the_host_accepts() {
    if skip_without_docker("a_configured_ceiling_changes_what_the_host_accepts") {
        return;
    }
    let fixture = SshdFixture::start(Topology::Normal);
    let transport = transport(&fixture);
    let target = TransportTarget::new(fixture.alias());
    let inbox = ensure_inbox(&transport, &target, None).unwrap();

    let tight = Limits::new(512, 1024, 20).unwrap();
    let attachment = sparse_attachment(&fixture, "shot.png", 513);

    let refused = plan_batch(&inbox, &SystemClock, &SystemIdSource).unwrap();
    let error = stage_attachments(&transport, &target, &refused, tight, &[attachment])
        .expect_err("513 bytes is over a 512 byte ceiling");
    assert_eq!(error.exit_code().as_u8(), 26);
    assert!(
        everything_under(&fixture, &inbox).is_empty(),
        "a configured ceiling must stop the batch as firmly as the default does"
    );

    // Same file, same host, roomier configuration: it goes.
    let allowed = plan_batch(&inbox, &SystemClock, &SystemIdSource).unwrap();
    let attachment = sparse_attachment(&fixture, "shot.png", 513);
    let staged = stage_attachments(
        &transport,
        &target,
        &allowed,
        Limits::new(1024, 2048, 20).unwrap(),
        &[attachment],
    )
    .expect("513 bytes is within a 1 KiB ceiling");
    assert_eq!(staged.files()[0].size(), 513);
}
