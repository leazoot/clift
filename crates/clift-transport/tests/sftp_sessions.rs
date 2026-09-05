//! One `sftp` session for the whole run, against a real SSH server.
//!
//! Connection reuse made four operations cost one authentication. It
//! did not change how many times the *server* is asked to start an
//! `sftp-server`, and that turned out to be where the time had gone: eleven
//! subsystem starts for one `clift send`.
//!
//! So that is the number counted here, and it is counted in sshd's own log --
//! `Starting session: subsystem 'sftp' ...`, one line per start. How many
//! processes Clift began is Clift's own word for it; how many subsystems the
//! server started is not.
//!
//! The second test in this file is the one the design rests on. Running a
//! batch inside a live session means Clift, not `sftp`, decides when a batch
//! has failed, and it decides by looking at whether the command wrote to
//! stderr. That is only sound if a successful command writes nothing there, so
//! every verb Clift actually uses is run successfully against a real server and
//! checked.

#![allow(clippy::unwrap_used, clippy::expect_used)]

#[path = "../../../tests/e2e/fixtures.rs"]
mod fixtures;

use clift_core::domain::RemotePath;
use clift_core::ports::TransportTarget;
use clift_transport::probe::OpenSshTransport;
use clift_transport::proc::{SftpBatch, SshRunner};
use fixtures::{SshdFixture, Topology, skip_without_docker};
use std::process::Command;

/// How many times the server was asked to start an sftp subsystem.
///
/// Requires `LogLevel VERBOSE` in the fixture's sshd configuration, which is
/// what makes this line appear at all.
fn sftp_sessions(fixture: &SshdFixture) -> usize {
    let output = Command::new("docker")
        .arg("logs")
        .arg(fixture.container())
        .output()
        .expect("docker logs must be runnable");
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    text.lines()
        .filter(|line| line.contains("subsystem 'sftp'"))
        .count()
}

fn home(fixture: &SshdFixture, suffix: &str) -> RemotePath {
    RemotePath::new(format!("{}/{suffix}", fixture.remote_home())).unwrap()
}

/// Six operations of the kind a send performs, so the two counts below are
/// comparable rather than each measuring their own thing.
fn six_operations(transport: &OpenSshTransport, fixture: &SshdFixture, tag: &str) {
    let target = TransportTarget::new(fixture.alias());
    let directory = home(fixture, tag);

    transport.resolve_home(&target).unwrap();
    transport.ensure_dir(&target, &directory, 0o700).unwrap();
    transport.stat(&target, &directory).unwrap();
    transport.list_dir(&target, &directory).unwrap();
    transport
        .stat(&target, &home(fixture, "not-there-at-all"))
        .unwrap();
    transport.remove(&target, &directory).unwrap();
}

/// The whole of a settled question in one number.
#[test]
fn a_run_costs_one_sftp_session_however_many_operations_it_makes() {
    if skip_without_docker("a_run_costs_one_sftp_session_however_many_operations_it_makes") {
        return;
    }
    let fixture = SshdFixture::start(Topology::Normal);
    let before = sftp_sessions(&fixture);

    let transport = OpenSshTransport::with_runner(
        SshRunner::new()
            .with_config_file(fixture.ssh_config())
            .with_sessions(),
    );
    six_operations(&transport, &fixture, "sessions-on");
    drop(transport);

    let after = sftp_sessions(&fixture);
    assert_eq!(
        after - before,
        1,
        "the operations started {} sftp subsystems; they are meant to share one",
        after - before
    );
}

/// The control: the same six operations without sessions.
///
/// It is what makes the number above mean something. It also shows why the
/// count is not six: several of these operations are more than one batch --
/// `ensure_dir` alone stats, creates and stats again.
#[test]
fn without_sessions_every_batch_starts_its_own_subsystem() {
    if skip_without_docker("without_sessions_every_batch_starts_its_own_subsystem") {
        return;
    }
    let fixture = SshdFixture::start(Topology::Normal);
    let before = sftp_sessions(&fixture);

    let transport =
        OpenSshTransport::with_runner(SshRunner::new().with_config_file(fixture.ssh_config()));
    six_operations(&transport, &fixture, "sessions-off");

    let after = sftp_sessions(&fixture);
    assert!(
        after - before >= 6,
        "six operations should have started at least six subsystems, not {}",
        after - before
    );
}

/// The assumption the session design rests on, checked against a real server.
///
/// Inside a session there is no per-command exit status to read, so "did this
/// command fail" is answered by "did it write to stderr". Every verb Clift
/// sends is run here in a way that must succeed; any one of them printing a
/// warning would make Clift call a successful operation a failure.
#[test]
fn no_verb_clift_uses_writes_to_stderr_when_it_succeeds() {
    if skip_without_docker("no_verb_clift_uses_writes_to_stderr_when_it_succeeds") {
        return;
    }
    let fixture = SshdFixture::start(Topology::Normal);
    let target = TransportTarget::new(fixture.alias());
    let runner = SshRunner::new()
        .with_config_file(fixture.ssh_config())
        .with_sessions();

    let local = fixture.workdir().join("payload.bin");
    // Large enough that `put` would have something to report a progress meter
    // about, if it were going to.
    std::fs::write(&local, vec![7_u8; 1024 * 1024]).unwrap();
    let directory = format!("{}/verbs", fixture.remote_home());

    let each: [(&'static str, Vec<String>); 9] = [
        ("pwd", vec![]),
        ("mkdir", vec![directory.clone()]),
        ("chmod", vec!["700".to_string(), directory.clone()]),
        (
            "put",
            vec![
                local.to_string_lossy().into_owned(),
                format!("{directory}/a"),
            ],
        ),
        ("chmod", vec!["600".to_string(), format!("{directory}/a")]),
        ("ls", vec!["-la".to_string(), directory.clone()]),
        (
            "rename",
            vec![format!("{directory}/a"), format!("{directory}/b")],
        ),
        ("rm", vec![format!("{directory}/b")]),
        ("rmdir", vec![directory.clone()]),
    ];

    for (verb, operands) in each {
        let mut batch = SftpBatch::new();
        let borrowed: Vec<&str> = operands.iter().map(String::as_str).collect();
        batch.push(verb, &borrowed).unwrap();
        let outcome = runner.run_sftp(&target, &batch).unwrap();
        assert_eq!(
            outcome.stderr, "",
            "`{verb}` succeeded but wrote to stderr, which is how a session \
             decides a command failed"
        );
        assert!(outcome.succeeded(), "`{verb}` was reported as failed");
    }
}

/// A batch stops at its first failure, exactly as a one-shot batch script does.
///
/// This is not a nicety. `ensure_dir` sends `mkdir` and `chmod` together, and a
/// `chmod` that ran after its `mkdir` had failed would be Clift changing the
/// permissions of a directory that was already there -- the one thing
/// `ensure_dir` documents that it will never do.
#[test]
fn a_batch_stops_at_its_first_failure() {
    if skip_without_docker("a_batch_stops_at_its_first_failure") {
        return;
    }
    let fixture = SshdFixture::start(Topology::Normal);
    let target = TransportTarget::new(fixture.alias());
    let runner = SshRunner::new()
        .with_config_file(fixture.ssh_config())
        .with_sessions();

    let after_the_failure = format!("{}/must-not-exist", fixture.remote_home());
    let mut batch = SftpBatch::new();
    batch
        .push("mkdir", &["/proc/no-such-parent/child"])
        .unwrap();
    batch.push("mkdir", &[after_the_failure.as_str()]).unwrap();

    let outcome = runner.run_sftp(&target, &batch).unwrap();
    assert!(!outcome.succeeded(), "the first command cannot have worked");
    assert!(
        !outcome.stderr.is_empty(),
        "a failure must carry the server's own words"
    );

    let mut look = SftpBatch::new();
    look.push("ls", &["-la", after_the_failure.as_str()])
        .unwrap();
    let found = runner.run_sftp(&target, &look).unwrap();
    assert!(
        !found.succeeded(),
        "the second command ran even though the first had failed"
    );
}

/// A session and a one-shot batch answer the same question the same way.
///
/// The session path parses text out of a stream instead of out of a finished
/// process, and this is what says the two agree.
#[test]
fn a_session_returns_what_a_one_shot_batch_would_have() {
    if skip_without_docker("a_session_returns_what_a_one_shot_batch_would_have") {
        return;
    }
    let fixture = SshdFixture::start(Topology::Normal);
    let target = TransportTarget::new(fixture.alias());

    let one_shot =
        OpenSshTransport::with_runner(SshRunner::new().with_config_file(fixture.ssh_config()));
    let session = OpenSshTransport::with_runner(
        SshRunner::new()
            .with_config_file(fixture.ssh_config())
            .with_sessions(),
    );

    assert_eq!(
        one_shot.resolve_home(&target).unwrap().as_str(),
        session.resolve_home(&target).unwrap().as_str()
    );

    let directory = home(&fixture, "compare");
    one_shot.ensure_dir(&target, &directory, 0o700).unwrap();

    let from_one_shot = one_shot.stat(&target, &directory).unwrap();
    let from_session = session.stat(&target, &directory).unwrap();
    assert_eq!(
        from_one_shot, from_session,
        "the same directory, read twice"
    );

    let missing = home(&fixture, "compare-absent");
    assert_eq!(one_shot.stat(&target, &missing).unwrap(), None);
    assert_eq!(
        session.stat(&target, &missing).unwrap(),
        None,
        "a missing path must be an answer in a session too, not an error"
    );

    one_shot.remove(&target, &directory).unwrap();
}
