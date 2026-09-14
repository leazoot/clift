//! One SFTP session for the whole run, against a real SSH server.
//!
//! Connection reuse made four operations cost one authentication where the
//! client supports it. It did not change how many times the *server* is asked
//! to start an `sftp-server`, and on a client without connection reuse --
//! Windows among them -- every operation was a new connection as well.
//!
//! Both numbers are counted here in sshd's own log: `Starting session:
//! subsystem 'sftp' ...` once per subsystem start, `Accepted publickey` once
//! per authentication. How many processes Clift began is Clift's own word for
//! it; what the server saw is not.

#![allow(clippy::unwrap_used, clippy::expect_used)]

#[path = "../../../tests/e2e/fixtures.rs"]
mod fixtures;

use clift_core::domain::{RemotePath, SafeFileName};
use clift_core::ports::TransportTarget;
use clift_transport::probe::OpenSshTransport;
use clift_transport::proc::SshRunner;
use fixtures::{SshdFixture, Topology, skip_without_docker};
use std::process::Command;
use std::time::{Duration, Instant};

fn server_log(fixture: &SshdFixture) -> String {
    let output = Command::new("docker")
        .arg("logs")
        .arg(fixture.container())
        .output()
        .expect("docker logs must be runnable");
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

/// How many times the server was asked to start an sftp subsystem.
///
/// Requires `LogLevel VERBOSE` in the fixture's sshd configuration, which is
/// what makes this line appear at all.
fn sftp_sessions(fixture: &SshdFixture) -> usize {
    server_log(fixture)
        .lines()
        .filter(|line| line.contains("subsystem 'sftp'"))
        .count()
}

/// How many times the server accepted a key. "Accepted publickey", not
/// "Accepted": at `LogLevel VERBOSE` sshd also logs "Accepted key ... found
/// at ..." while it is still deciding.
fn authentications(fixture: &SshdFixture) -> usize {
    server_log(fixture)
        .lines()
        .filter(|line| line.contains("Accepted publickey"))
        .count()
}

/// How many sessions the server has closed.
fn closed_sessions(fixture: &SshdFixture) -> usize {
    server_log(fixture)
        .lines()
        .filter(|line| line.contains("Close session"))
        .count()
}

/// Sends a signal to every `sftp-server` in the container.
///
/// This is how a server that stops answering is produced for real: the
/// process that serves the session is paused, so the request is genuinely
/// unanswered rather than simulated. The image has no `pkill`, so the processes
/// are found through `/proc`.
fn signal_sftp_servers(fixture: &SshdFixture, signal: &str) {
    let script = format!(
        "for p in /proc/[0-9]*; do \
         [ \"$(cat $p/comm 2>/dev/null)\" = sftp-server ] && kill -{signal} \"${{p#/proc/}}\"; \
         done; true"
    );
    let status = Command::new("docker")
        .args(["exec", fixture.container(), "sh", "-c", &script])
        .status()
        .expect("docker exec must be runnable");
    assert!(status.success(), "could not send {signal} to sftp-server");
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
/// It is what makes the number above mean something.
#[test]
fn without_sessions_every_operation_starts_its_own_subsystem() {
    if skip_without_docker("without_sessions_every_operation_starts_its_own_subsystem") {
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

/// Everything the remote side of a send does, on a client that cannot reuse
/// connections, costs one authentication.
///
/// No `ControlMaster` is configured here, which is exactly the position of a
/// Windows client: the only thing standing between one send and a dozen
/// logins is the session itself.
#[test]
fn a_send_costs_one_authentication_without_connection_reuse() {
    if skip_without_docker("a_send_costs_one_authentication_without_connection_reuse") {
        return;
    }
    let fixture = SshdFixture::start(Topology::Normal);
    let target = TransportTarget::new(fixture.alias());
    let local = fixture.workdir().join("shot.png");
    std::fs::write(&local, vec![9_u8; 200_000]).unwrap();

    let logins_before = authentications(&fixture);
    let sessions_before = sftp_sessions(&fixture);

    let transport = OpenSshTransport::with_runner(
        SshRunner::new()
            .with_config_file(fixture.ssh_config())
            .with_sessions(),
    );
    let root = home(&fixture, ".cache/clift/inbox");
    let batch = home(&fixture, ".cache/clift/inbox/2026-09-14/0123456789abcdef");
    let destination = batch.join(&SafeFileName::new("shot.png").unwrap());

    transport.resolve_home(&target).unwrap();
    transport.ensure_dir(&target, &root, 0o700).unwrap();
    transport.ensure_dir(&target, &batch, 0o700).unwrap();
    let sent = transport
        .upload_atomic(&target, &local, &destination)
        .unwrap();
    drop(transport);

    assert_eq!(sent, 200_000);
    assert_eq!(
        authentications(&fixture) - logins_before,
        1,
        "one send authenticated more than once"
    );
    assert_eq!(sftp_sessions(&fixture) - sessions_before, 1);
}

/// A request the server never answers is reported, never sent again.
///
/// The server's `sftp-server` is paused after the session is open, so the
/// request is stuck on the server itself. Starting over would mean a new
/// session, which would show up in sshd's log; for a `rename`, a second
/// attempt is a second rename.
#[test]
fn a_request_the_server_never_answers_is_reported_and_not_sent_again() {
    if skip_without_docker("a_request_the_server_never_answers_is_reported_and_not_sent_again") {
        return;
    }
    let fixture = SshdFixture::start(Topology::Normal);
    let target = TransportTarget::new(fixture.alias());
    let timeout = Duration::from_secs(2);
    let transport = OpenSshTransport::with_runner(
        SshRunner::new()
            .with_config_file(fixture.ssh_config())
            .with_sessions()
            .with_timeout(timeout),
    );
    transport.resolve_home(&target).unwrap();

    let before = sftp_sessions(&fixture);
    signal_sftp_servers(&fixture, "STOP");
    let started = Instant::now();
    let outcome = transport.remove(&target, &home(&fixture, "anything"));
    let elapsed = started.elapsed();
    signal_sftp_servers(&fixture, "CONT");
    let after = sftp_sessions(&fixture);

    let error = outcome.expect_err("an unanswered request cannot succeed");
    assert_eq!(
        after - before,
        0,
        "the unanswered request was sent again in a new session after {elapsed:?}: {error}"
    );
    assert!(
        elapsed < timeout * 2,
        "one timeout is the whole wait, not one per attempt: {elapsed:?}"
    );
    assert_eq!(
        error.exit_code().as_u8(),
        22,
        "a stopped connection is a connection failure: {error}"
    );
}

/// A kept session that has ended since its last use is replaced before
/// anything is sent on it, so a server that closed an idle session between two
/// operations costs a reconnection, not a failed send.
#[test]
fn a_session_that_has_ended_is_replaced_before_anything_is_sent_on_it() {
    if skip_without_docker("a_session_that_has_ended_is_replaced_before_anything_is_sent_on_it") {
        return;
    }
    let fixture = SshdFixture::start(Topology::Normal);
    let target = TransportTarget::new(fixture.alias());
    let transport = OpenSshTransport::with_runner(
        SshRunner::new()
            .with_config_file(fixture.ssh_config())
            .with_sessions(),
    );
    transport.resolve_home(&target).unwrap();

    let before = sftp_sessions(&fixture);
    signal_sftp_servers(&fixture, "TERM");
    // Long enough for `ssh` to see its channel close and exit.
    std::thread::sleep(Duration::from_secs(2));

    let found = transport
        .stat(&target, &home(&fixture, "not-there"))
        .expect("the next operation must get a working session");
    assert_eq!(found, None);
    assert_eq!(sftp_sessions(&fixture) - before, 1);
}

/// A kept session and a fresh one answer the same question the same way.
#[test]
fn a_kept_session_returns_what_a_fresh_one_would_have() {
    if skip_without_docker("a_kept_session_returns_what_a_fresh_one_would_have") {
        return;
    }
    let fixture = SshdFixture::start(Topology::Normal);
    let target = TransportTarget::new(fixture.alias());

    let fresh =
        OpenSshTransport::with_runner(SshRunner::new().with_config_file(fixture.ssh_config()));
    let kept = OpenSshTransport::with_runner(
        SshRunner::new()
            .with_config_file(fixture.ssh_config())
            .with_sessions(),
    );

    assert_eq!(
        fresh.resolve_home(&target).unwrap().as_str(),
        kept.resolve_home(&target).unwrap().as_str()
    );

    let directory = home(&fixture, "compare");
    fresh.ensure_dir(&target, &directory, 0o700).unwrap();
    assert_eq!(
        fresh.stat(&target, &directory).unwrap(),
        kept.stat(&target, &directory).unwrap(),
        "the same directory, read twice"
    );

    let missing = home(&fixture, "compare-absent");
    assert_eq!(fresh.stat(&target, &missing).unwrap(), None);
    assert_eq!(
        kept.stat(&target, &missing).unwrap(),
        None,
        "a missing path must be an answer in a kept session too, not an error"
    );

    fresh.remove(&target, &directory).unwrap();
}

/// Operations made within the idle limit share the session the first one
/// opened, which is what makes a second key press quick.
#[test]
fn operations_within_the_idle_limit_share_one_session() {
    if skip_without_docker("operations_within_the_idle_limit_share_one_session") {
        return;
    }
    let fixture = SshdFixture::start(Topology::Normal);
    let target = TransportTarget::new(fixture.alias());
    let transport = OpenSshTransport::with_runner(
        SshRunner::new()
            .with_config_file(fixture.ssh_config())
            .with_sessions()
            .with_idle_limit(Duration::from_secs(30)),
    );
    let before = sftp_sessions(&fixture);
    transport.resolve_home(&target).unwrap();
    std::thread::sleep(Duration::from_secs(1));
    transport.tend_sessions();
    transport.resolve_home(&target).unwrap();
    assert_eq!(sftp_sessions(&fixture) - before, 1);
}

/// A kept session that goes unused past its limit is closed rather than kept
/// for the life of the process, and the next operation opens a fresh one.
#[test]
fn a_kept_session_is_closed_once_it_has_been_idle_past_its_limit() {
    if skip_without_docker("a_kept_session_is_closed_once_it_has_been_idle_past_its_limit") {
        return;
    }
    let fixture = SshdFixture::start(Topology::Normal);
    let target = TransportTarget::new(fixture.alias());
    let transport = OpenSshTransport::with_runner(
        SshRunner::new()
            .with_config_file(fixture.ssh_config())
            .with_sessions()
            .with_idle_limit(Duration::from_secs(1)),
    );
    transport.resolve_home(&target).unwrap();
    let opened = sftp_sessions(&fixture);
    let closed = closed_sessions(&fixture);

    std::thread::sleep(Duration::from_millis(1_500));
    transport.tend_sessions();
    // The server logs the close once the client has gone, which takes a
    // moment after the process is stopped.
    let deadline = Instant::now() + Duration::from_secs(10);
    while closed_sessions(&fixture) == closed && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(200));
    }
    assert!(
        closed_sessions(&fixture) > closed,
        "the idle session was left open"
    );

    transport.resolve_home(&target).unwrap();
    assert_eq!(
        sftp_sessions(&fixture) - opened,
        1,
        "the next operation opens a fresh session"
    );
}
