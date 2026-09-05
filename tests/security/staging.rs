//! The staging layer, from the attacker's side.
//!
//! These are not extra assertions on the happy path. Each test states a way
//! The specification could be broken and shows that it is not: a file name that tries to
//! leave the batch directory, a symbolic link that points out of the inbox,
//! five batches racing for the same name, and an error message that carries the
//! contents of an attachment out to the terminal.
//!
//! Everything here runs against a real OpenSSH server in a throwaway container,
//! because a claim about path handling that is only tested against a fake is a
//! claim about the fake.

#![allow(clippy::unwrap_used, clippy::expect_used)]

#[path = "../e2e/fixtures.rs"]
mod fixtures;

use clift_core::domain::{
    BATCH_ID_BYTES, BatchId, FileKind, Limits, LocalAttachment, RemotePath, SafeFileName,
};
use clift_core::error::{CliftError, ErrorKind, Stage};
use clift_core::ports::{Clock, IdSource, RemoteEntryKind, TransportTarget};
use clift_core::staging::{ensure_inbox, plan_batch};
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

fn attachment(
    fixture: &SshdFixture,
    local: &str,
    claimed_name: &str,
    body: &[u8],
) -> LocalAttachment {
    let path: PathBuf = fixture.workdir().join(local);
    std::fs::write(&path, body).expect("the local attachment must be writable");
    let size = std::fs::metadata(&path).expect("it exists").len();
    // The name is what the clipboard or the filesystem claimed it is; sanitise
    // is where a hostile one stops being dangerous.
    LocalAttachment::new(
        path,
        SafeFileName::sanitize(claimed_name),
        size,
        FileKind::Regular,
    )
    .expect("a regular file at an absolute path")
}

/// The remote path with `..`, `.` and duplicate separators resolved by the
/// server itself. Asking the server is the point: our own idea of what a path
/// means is exactly what is under test.
fn canonical(fixture: &SshdFixture, path: &RemotePath) -> String {
    let output = fixture.ssh(&format!("readlink -f \"{path}\""));
    assert!(output.status.success(), "readlink -f failed for {path}");
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

/// Acceptance 1: a hostile file name must not be able to place a file outside
/// the batch directory, whatever it looks like.
#[test]
fn a_traversing_file_name_cannot_leave_the_batch_directory() {
    if skip_without_docker("a_traversing_file_name_cannot_leave_the_batch_directory") {
        return;
    }
    let fixture = SshdFixture::start(Topology::Normal);
    let transport = transport(&fixture);
    let target = TransportTarget::new(fixture.alias());
    let inbox = ensure_inbox(&transport, &target, None).unwrap();
    let plan = plan_batch(&inbox, &SystemClock, &SystemIdSource).unwrap();

    let hostile = [
        "../../../etc/passwd",
        "..",
        "....//....//etc/shadow",
        "/etc/passwd",
        "..\\..\\windows\\system32\\config\\sam",
        "-rf",
        "shot.png/../../escape.png",
    ];

    let attachments: Vec<LocalAttachment> = hostile
        .iter()
        .enumerate()
        .map(|(index, name)| attachment(&fixture, &format!("hostile-{index}"), name, b"payload"))
        .collect();

    let staged =
        stage_attachments(&transport, &target, &plan, Limits::default(), &attachments).unwrap();

    let batch_root = canonical(&fixture, plan.directory());
    for file in staged.files() {
        assert!(
            file.path().is_within(plan.directory()),
            "{} is outside the batch directory before the server even sees it",
            file.path()
        );
        // And after the server has had its say: the file that actually exists
        // resolves to somewhere inside the batch directory.
        let resolved = canonical(&fixture, file.path());
        assert!(
            resolved.starts_with(&format!("{batch_root}/")),
            "{resolved} escaped {batch_root}"
        );
    }

    // Nothing was created above the batch directory either.
    let above = fixture.ssh(&format!("ls -A \"{}\"", inbox.root()));
    let listed = String::from_utf8_lossy(&above.stdout);
    assert_eq!(
        listed.lines().filter(|line| !line.is_empty()).count(),
        1,
        "only the date directory may exist under the inbox root: {listed}"
    );
    assert!(!listed.contains("passwd"), "{listed}");
    assert!(!listed.contains("escape"), "{listed}");
}

/// Acceptance 2: a symbolic link pointing out of the inbox is reported as a
/// link and never walked through.
#[test]
fn a_symlink_out_of_the_inbox_is_not_followed() {
    if skip_without_docker("a_symlink_out_of_the_inbox_is_not_followed") {
        return;
    }
    let fixture = SshdFixture::start(Topology::Normal);
    let transport = transport(&fixture);
    let target = TransportTarget::new(fixture.alias());
    let inbox = ensure_inbox(&transport, &target, None).unwrap();
    let plan = plan_batch(&inbox, &SystemClock, &SystemIdSource).unwrap();

    let attachment = attachment(&fixture, "shot.png", "shot.png", b"a real attachment");
    stage_attachments(&transport, &target, &plan, Limits::default(), &[attachment]).unwrap();

    // Planted by someone else with access to the account: a link out of the
    // inbox, sitting in the batch directory next to a real file.
    let planted = fixture.ssh(&format!("ln -s /etc \"{}/etcetera\"", plan.directory()));
    assert!(planted.status.success(), "the link must be plantable");

    let entries = transport.list_dir(&target, plan.directory()).unwrap();
    let names: Vec<&str> = entries.iter().map(|entry| entry.name.as_str()).collect();
    assert!(names.contains(&"shot.png"), "{names:?}");
    assert!(names.contains(&"etcetera"), "{names:?}");

    let link = entries
        .iter()
        .find(|entry| entry.name.as_str() == "etcetera")
        .expect("the link is listed");
    assert_eq!(
        link.kind,
        RemoteEntryKind::Symlink,
        "a link must be reported as a link, not as the directory it points at"
    );

    // The listing is of the batch directory, not of /etc. If the link had been
    // followed, the contents of /etc would be in here.
    assert_eq!(
        entries.len(),
        2,
        "the listing walked through the link: {names:?}"
    );
    assert!(
        !names.contains(&"passwd"),
        "the contents of /etc leaked into the listing: {names:?}"
    );
}

/// Acceptance 3: five batches uploading a file of the same name at the same
/// time stay entirely separate. This is what the random batch directory is for.
#[test]
fn five_concurrent_batches_of_the_same_name_do_not_touch_each_other() {
    if skip_without_docker("five_concurrent_batches_of_the_same_name_do_not_touch_each_other") {
        return;
    }
    let fixture = SshdFixture::start(Topology::Normal);
    let transport = transport(&fixture);
    let target = TransportTarget::new(fixture.alias());
    let inbox = ensure_inbox(&transport, &target, None).unwrap();

    let bodies: Vec<String> = (0..5)
        .map(|index| format!("the contents of batch number {index}"))
        .collect();

    let staged: Vec<_> = std::thread::scope(|scope| {
        let handles: Vec<_> = bodies
            .iter()
            .enumerate()
            .map(|(index, body)| {
                let transport = &transport;
                let target = &target;
                let inbox = &inbox;
                let fixture = &fixture;
                scope.spawn(move || {
                    let plan = plan_batch(inbox, &SystemClock, &SystemIdSource).unwrap();
                    let attachment = attachment(
                        fixture,
                        &format!("source-{index}.png"),
                        "shot.png",
                        body.as_bytes(),
                    );
                    stage_attachments(transport, target, &plan, Limits::default(), &[attachment])
                        .unwrap()
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|handle| handle.join().expect("no batch may panic"))
            .collect()
    });

    let mut directories: Vec<String> = staged
        .iter()
        .map(|batch| batch.directory().as_str().to_string())
        .collect();
    directories.sort();
    directories.dedup();
    assert_eq!(directories.len(), 5, "two batches shared a directory");

    for (batch, body) in staged.iter().zip(&bodies) {
        let path = batch.files()[0].path();
        assert!(path.as_str().ends_with("/shot.png"), "{path}");
        let read = fixture.ssh(&format!("cat \"{path}\""));
        assert_eq!(
            String::from_utf8_lossy(&read.stdout),
            body.as_str(),
            "batch contents were crossed over"
        );
        let mode = fixture.ssh(&format!("stat -c %a \"{path}\""));
        assert_eq!(String::from_utf8_lossy(&mode.stdout).trim(), "600");
    }
}

/// Acceptance 4: the contents of an attachment must never come back out in an
/// error, a cause chain or a debug rendering.
///
/// The send command that would print these did not exist yet when this was
/// written; what does exist is the layer where the bytes are handled, and
/// that is where a leak would originate. Every error this test can provoke is
/// rendered three ways -- display, debug and the full cause chain -- and
/// searched for the marker.
#[test]
fn attachment_contents_never_appear_in_an_error_or_a_debug_rendering() {
    if skip_without_docker("attachment_contents_never_appear_in_an_error_or_a_debug_rendering") {
        return;
    }
    const MARKER: &str = "CLIFT-CANARY-e4a1c0-do-not-print-me";

    let fixture = SshdFixture::start(Topology::SmallQuota);
    let transport = transport(&fixture);
    let target = TransportTarget::new(fixture.alias());

    // The quota filesystem, so the write genuinely fails partway through.
    let directory = RemotePath::new(format!("{}/quota/batch", fixture.remote_home())).unwrap();
    transport.ensure_dir(&target, &directory, 0o700).unwrap();

    let mut body = MARKER.as_bytes().to_vec();
    body.resize(3 * 1024 * 1024, b'x');
    body.extend_from_slice(MARKER.as_bytes());
    let source = fixture.workdir().join("canary.bin");
    std::fs::write(&source, &body).expect("the local attachment must be writable");

    let destination = directory.join(&SafeFileName::new("canary.bin").unwrap());
    let error = transport
        .upload_atomic(&target, &source, &destination)
        .expect_err("3 MiB does not fit in 1 MiB");

    let renderings = [
        error.to_string(),
        format!("{error:?}"),
        error.cause_chain().join("\n"),
        error
            .remedy()
            .map(|remedy| format!("{} {}", remedy.description(), remedy.command()))
            .unwrap_or_default(),
    ];
    for rendering in &renderings {
        assert!(
            !rendering.contains(MARKER),
            "an attachment's contents reached the output: {rendering}"
        );
        assert!(
            !rendering.contains("xxxxxxxxxx"),
            "an attachment's contents reached the output: {rendering}"
        );
    }

    // The same for the value handed back on the success path: a StagedBatch is
    // what a command renders, and it must carry paths, not payloads.
    let inbox = ensure_inbox(&transport, &target, None).unwrap();
    let plan = plan_batch(&inbox, &SystemClock, &SystemIdSource).unwrap();
    let small = attachment(&fixture, "small.bin", "small.bin", MARKER.as_bytes());
    let staged =
        stage_attachments(&transport, &target, &plan, Limits::default(), &[small]).unwrap();
    let rendered = format!("{staged:?}");
    assert!(
        !rendered.contains(MARKER),
        "the staged batch carries the attachment's contents: {rendered}"
    );
}
