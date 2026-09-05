//! Atomic upload against a real OpenSSH server.
//!
//! The unit tests decide what the intermediate name looks like and what a size
//! mismatch does. Only a server can answer the questions that matter here: are
//! the permissions really 0600, does an interrupted transfer really leave the
//! final name absent, and does a batch directory really keep two files of the
//! same name apart.

#![allow(clippy::unwrap_used, clippy::expect_used)]

#[path = "../../../tests/e2e/fixtures.rs"]
mod fixtures;

use clift_core::domain::{BATCH_ID_BYTES, BatchId, RemotePath, SafeFileName};
use clift_core::error::{CliftError, ErrorKind, Stage};
use clift_core::ports::{Clock, IdSource, RemoteUpload, TransportTarget};
use clift_core::staging::{BatchPlan, INBOX_MODE, create_batch, ensure_inbox, plan_batch};
use clift_transport::probe::OpenSshTransport;
use clift_transport::proc::SshRunner;
use fixtures::{SshdFixture, Topology, skip_without_docker};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

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

/// A batch directory on the server, ready to receive uploads.
fn prepared_batch(fixture: &SshdFixture, transport: &OpenSshTransport) -> BatchPlan {
    let target = TransportTarget::new(fixture.alias());
    let inbox = ensure_inbox(transport, &target, None).unwrap();
    let plan = plan_batch(&inbox, &SystemClock, &SystemIdSource).unwrap();
    create_batch(transport, &target, &plan).unwrap();
    plan
}

fn local_file(fixture: &SshdFixture, name: &str, contents: &[u8]) -> PathBuf {
    let path = fixture.workdir().join(name);
    std::fs::write(&path, contents).expect("the local attachment must be writable");
    path
}

fn destination(plan: &BatchPlan, name: &str) -> RemotePath {
    plan.file(&SafeFileName::new(name).expect("the test name must be valid"))
}

fn remote_mode(fixture: &SshdFixture, path: &RemotePath) -> String {
    let output = fixture.ssh(&format!("stat -c %a \"{path}\""));
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

fn remote_bytes(fixture: &SshdFixture, path: &RemotePath) -> Vec<u8> {
    let output = fixture.ssh(&format!("cat \"{path}\""));
    assert!(output.status.success(), "could not read back {path}");
    output.stdout
}

fn exists(fixture: &SshdFixture, path: &RemotePath) -> bool {
    fixture.ssh(&format!("test -e \"{path}\"")).status.success()
}

/// Every name in a directory, one per line.
fn entries(fixture: &SshdFixture, directory: &RemotePath) -> Vec<String> {
    let output = fixture.ssh(&format!("ls -A \"{directory}\""));
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::to_string)
        .filter(|line| !line.is_empty())
        .collect()
}

/// Acceptance criteria 1 and 5, plus the byte-for-byte arrival that makes the
/// size check meaningful.
#[test]
fn an_uploaded_attachment_is_private_intact_and_leaves_nothing_behind() {
    if skip_without_docker("an_uploaded_attachment_is_private_intact_and_leaves_nothing_behind") {
        return;
    }
    let fixture = SshdFixture::start(Topology::Normal);
    let transport = transport(&fixture);
    let target = TransportTarget::new(fixture.alias());
    let plan = prepared_batch(&fixture, &transport);

    // Every byte value, so a mangled transfer shows up rather than surviving
    // as readable text.
    let contents: Vec<u8> = (0..=255u8).cycle().take(9_000).collect();
    // World readable on purpose: the upload has to tighten it, not inherit it.
    let source = local_file(&fixture, "shot.png", &contents);
    let target_path = destination(&plan, "shot.png");

    let reported = transport
        .upload_atomic(&target, &source, &target_path)
        .expect("the upload must succeed on the normal topology");

    assert_eq!(reported, contents.len() as u64);
    assert_eq!(remote_mode(&fixture, &target_path), "600");
    assert_eq!(remote_bytes(&fixture, &target_path), contents);
    assert_eq!(
        entries(&fixture, plan.directory()),
        vec!["shot.png".to_string()],
        "a successful upload leaves no intermediate file behind"
    );

    // Acceptance criterion 5: nothing about zero bytes is a special case.
    let empty_source = local_file(&fixture, "empty.txt", b"");
    let empty_path = destination(&plan, "empty.txt");
    assert_eq!(
        transport
            .upload_atomic(&target, &empty_source, &empty_path)
            .expect("a zero byte file is still a file"),
        0
    );
    assert_eq!(remote_mode(&fixture, &empty_path), "600");
    assert!(remote_bytes(&fixture, &empty_path).is_empty());
}

/// Acceptance criterion 4: the batch directory is what keeps two attachments of
/// the same name apart. Nothing is overwritten, because nothing collides.
#[test]
fn the_same_file_name_in_two_batches_does_not_overwrite() {
    if skip_without_docker("the_same_file_name_in_two_batches_does_not_overwrite") {
        return;
    }
    let fixture = SshdFixture::start(Topology::Normal);
    let transport = transport(&fixture);
    let target = TransportTarget::new(fixture.alias());

    let first = prepared_batch(&fixture, &transport);
    let second = prepared_batch(&fixture, &transport);
    assert_ne!(first.directory(), second.directory());

    let first_source = local_file(&fixture, "one.png", b"the first screenshot");
    let second_source = local_file(&fixture, "two.png", b"a different screenshot");
    let first_path = destination(&first, "shot.png");
    let second_path = destination(&second, "shot.png");

    transport
        .upload_atomic(&target, &first_source, &first_path)
        .unwrap();
    transport
        .upload_atomic(&target, &second_source, &second_path)
        .unwrap();

    assert_eq!(remote_bytes(&fixture, &first_path), b"the first screenshot");
    assert_eq!(
        remote_bytes(&fixture, &second_path),
        b"a different screenshot"
    );
}

/// Acceptance criterion 2: an interrupted transfer must not leave the final
/// name in place, because that name is what would be handed to an agent.
///
/// The interruption is real: the sftp child is killed while it is writing. The
/// test watches the batch directory from another thread as that happens, so it
/// can prove the transfer had genuinely started -- an upload that never reached
/// the server would satisfy "the final name is absent" without meaning anything.
#[test]
fn an_interrupted_transfer_never_leaves_the_final_name_behind() {
    if skip_without_docker("an_interrupted_transfer_never_leaves_the_final_name_behind") {
        return;
    }
    let fixture = SshdFixture::start(Topology::Normal);
    let transport = transport(&fixture);
    let target = TransportTarget::new(fixture.alias());
    let plan = prepared_batch(&fixture, &transport);

    // A gigabyte on the wire against a second and a half: even a loopback
    // connection to a container needs several seconds for that, so the transfer
    // is certain to be cut short rather than merely likely to be.
    let interrupted = OpenSshTransport::with_runner(
        SshRunner::new()
            .with_config_file(fixture.ssh_config())
            .with_timeout(Duration::from_millis(1_500)),
    );

    let source = fixture.workdir().join("huge.bin");
    let handle = std::fs::File::create(&source).expect("the local file must be creatable");
    handle
        .set_len(1024 * 1024 * 1024)
        .expect("sparse allocation");
    drop(handle);

    let target_path = destination(&plan, "huge.bin");
    let mut seen_in_flight: Vec<String> = Vec::new();

    let outcome = std::thread::scope(|scope| {
        let upload = scope.spawn(|| interrupted.upload_atomic(&target, &source, &target_path));
        while !upload.is_finished() {
            for entry in entries(&fixture, plan.directory()) {
                if !seen_in_flight.contains(&entry) {
                    seen_in_flight.push(entry);
                }
            }
        }
        upload.join().expect("the upload thread must not panic")
    });

    let error = outcome.expect_err(
        "the transfer finished inside the timeout, so nothing was interrupted and this test          proved nothing; the file needs to be larger or the timeout shorter",
    );
    assert_eq!(
        error.exit_code().as_u8(),
        22,
        "a stopped transfer is a connection failure: {error}"
    );

    assert!(
        seen_in_flight
            .iter()
            .any(|entry| entry.starts_with('.') && entry.ends_with(".part")),
        "no intermediate file was ever observed, so nothing was actually interrupted: \
         {seen_in_flight:?}"
    );
    assert!(
        !seen_in_flight.iter().any(|entry| entry == "huge.bin"),
        "the final name appeared while the transfer was still running: {seen_in_flight:?}"
    );
    assert!(
        !exists(&fixture, &target_path),
        "the final name must not exist after an interrupted transfer"
    );
    for entry in entries(&fixture, plan.directory()) {
        assert!(
            entry.starts_with('.') && entry.ends_with(".part"),
            "only an intermediate file may survive an interruption, found {entry}"
        );
    }
}

/// Acceptance criterion 3: a write that cannot complete produces exit code 23,
/// removes the intermediate file, and hands back no path at all.
///
/// The failure is real, not injected: the destination is on a 1 MiB filesystem
/// and the attachment is 3 MiB. SFTP protocol 3 has no code for "out of space",
/// so the server answers with a plain failure -- which is exactly why Clift
/// must treat any incomplete write this way rather than looking for ENOSPC.
#[test]
fn a_write_that_cannot_complete_leaves_no_path_and_no_intermediate_file() {
    if skip_without_docker("a_write_that_cannot_complete_leaves_no_path_and_no_intermediate_file") {
        return;
    }
    let fixture = SshdFixture::start(Topology::SmallQuota);
    let transport = transport(&fixture);
    let target = TransportTarget::new(fixture.alias());

    // The size limited filesystem, not the inbox: this test is about what a
    // failing write does, and the inbox lives on the roomy filesystem.
    let directory = RemotePath::new(format!("{}/quota/batch", fixture.remote_home())).unwrap();
    transport
        .ensure_dir(&target, &directory, INBOX_MODE)
        .expect("the quota filesystem is writable, it is merely small");

    let source = local_file(&fixture, "big.bin", &vec![0u8; 3 * 1024 * 1024]);
    let target_path = directory.join(&SafeFileName::new("big.bin").unwrap());

    let error = transport
        .upload_atomic(&target, &source, &target_path)
        .expect_err("3 MiB cannot fit in 1 MiB");

    assert_eq!(
        error.exit_code().as_u8(),
        23,
        "an incomplete write is a transfer failure: {error}"
    );
    let rendered = format!(
        "{error} {}",
        error
            .remedy()
            .map(|remedy| format!("{} {}", remedy.description(), remedy.command()))
            .unwrap_or_default()
    );
    assert!(
        !rendered.contains(target_path.as_str()),
        "a failed upload must not hand out a remote path: {rendered}"
    );

    assert!(
        !exists(&fixture, &target_path),
        "the final name must not exist after a failed write"
    );
    assert!(
        entries(&fixture, &directory).is_empty(),
        "the intermediate file must have been removed: {:?}",
        entries(&fixture, &directory)
    );
}

/// Guards the signature rather than the behaviour: `Path` is what the port
/// promises, and a change to it would silently move the local-filesystem
/// concern out of the adapter.
#[test]
fn the_upload_port_is_implemented_by_the_openssh_transport() {
    fn accepts<T: RemoteUpload>(_: &T, _: &Path) {}
    accepts(&OpenSshTransport::new(), Path::new("/tmp/x"));
}
