//! One connection, reused, against a real SSH server.
//!
//! The claim these tests exist for is not "Clift passes three options" -- the
//! unit tests cover that, and passing an option proves nothing about what the
//! server saw. It is **the server accepts one connection where it used to
//! accept nine**, so that is what is counted here, in sshd's own log.
//!
//! The other half is the specification's prohibition: a user who already multiplexes must
//! be left alone. That is checked the same way, by looking at which socket
//! actually appeared.

#![allow(clippy::unwrap_used, clippy::expect_used)]

#[path = "../../../tests/e2e/fixtures.rs"]
mod fixtures;

use clift_core::domain::RemotePath;
use clift_core::ports::TransportTarget;
use clift_transport::probe::OpenSshTransport;
use clift_transport::proc::SshRunner;
use clift_transport::reuse::Reuse;
use fixtures::{SshdFixture, Topology, skip_without_docker};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

/// Short enough that a control socket path fits in `sun_path`, and inside the
/// repository rather than in a world-writable directory -- the same rule the
/// product follows, for the same reason.
fn socket_dir(name: &str) -> PathBuf {
    // Deliberately terse. A control socket path has 57 characters of hash and
    // temporary suffix appended to it, and `target/connection-reuse/...` would
    // spend the budget on a name nobody reads.
    let directory = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/m")
        .join(format!("{name}{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&directory);
    std::fs::create_dir_all(&directory).unwrap();
    std::fs::canonicalize(&directory).unwrap()
}

fn reuse(directory: &Path) -> Reuse {
    // Five seconds: long enough that the operations in one test share a
    // master, short enough that nothing is left running after it.
    Reuse::in_directory(directory, Duration::from_secs(5)).unwrap_or_else(|error| {
        panic!(
            "the socket path under {} is too long for this machine: {error}",
            directory.display()
        )
    })
}

/// Every socket in a directory, so "did one appear" is answerable.
fn sockets(directory: &Path) -> Vec<String> {
    std::fs::read_dir(directory)
        .unwrap()
        .filter_map(Result::ok)
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect()
}

/// How many times sshd has accepted an authentication, from its own log.
///
/// The container runs sshd in the foreground, so its log is the container's
/// output. This is the one number that cannot be faked by the client side.
fn accepted_connections(fixture: &SshdFixture) -> usize {
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
        // "Accepted publickey", not "Accepted": at LogLevel VERBOSE sshd also
        // logs "Accepted key ... found at ..." while it is still deciding, and
        // counting that too would double every answer here.
        .filter(|line| line.contains("Accepted publickey"))
        .count()
}

/// Closes the master, so no test leaves a connection behind.
fn close_master(fixture: &SshdFixture, directory: &Path) {
    let mut control = std::ffi::OsString::from("ControlPath=");
    control.push(directory.join("%C"));
    let _ = Command::new("ssh")
        .arg("-F")
        .arg(fixture.ssh_config())
        .arg("-o")
        .arg(control)
        .arg("-O")
        .arg("exit")
        .arg(fixture.alias())
        .output();
}

fn home(fixture: &SshdFixture, suffix: &str) -> RemotePath {
    RemotePath::new(format!("{}/{suffix}", fixture.remote_home())).unwrap()
}

/// The whole of the specification in one number: four operations, one authentication.
#[test]
fn four_operations_cost_one_authentication_instead_of_four() {
    if skip_without_docker("four_operations_cost_one_authentication_instead_of_four") {
        return;
    }
    let fixture = SshdFixture::start(Topology::Normal);
    let target = TransportTarget::new(fixture.alias());
    let directory = socket_dir("r");

    // The fixture authenticated once while waiting for the server to come up,
    // so the baseline is whatever that left behind rather than zero.
    let before = accepted_connections(&fixture);
    assert!(
        before > 0,
        "sshd's log does not say when it accepts a connection, so this test \
         cannot measure anything"
    );

    let transport = OpenSshTransport::with_runner(
        SshRunner::new()
            .with_config_file(fixture.ssh_config())
            .with_reuse(reuse(&directory)),
    );

    for index in 0..4 {
        transport
            .ensure_dir(&target, &home(&fixture, &format!("reuse-{index}")), 0o700)
            .unwrap();
    }

    let after = accepted_connections(&fixture);
    assert_eq!(
        after - before,
        1,
        "four operations authenticated {} times; they are meant to share one connection",
        after - before
    );
    // Not a round number by accident: see the control test below, where the
    // same four operations cost sixteen.
    assert_eq!(
        sockets(&directory).len(),
        1,
        "exactly one control socket, named for the destination: {:?}",
        sockets(&directory)
    );

    close_master(&fixture, &directory);
}

/// The master is not a daemon: it goes away by itself.
///
/// The specification promises nothing of Clift's is running when Clift is not, and a
/// reused connection is the one thing that could quietly break that promise.
/// `ControlPersist` is what keeps it honest, so it is worth watching happen
/// once rather than trusting the option name.
#[test]
fn the_master_closes_itself_once_it_has_been_idle() {
    if skip_without_docker("the_master_closes_itself_once_it_has_been_idle") {
        return;
    }
    let fixture = SshdFixture::start(Topology::Normal);
    let directory = socket_dir("i");
    let transport = OpenSshTransport::with_runner(
        SshRunner::new()
            .with_config_file(fixture.ssh_config())
            .with_reuse(reuse(&directory)),
    );
    transport
        .ensure_dir(
            &TransportTarget::new(fixture.alias()),
            &home(&fixture, "idle"),
            0o700,
        )
        .unwrap();
    assert_eq!(sockets(&directory).len(), 1, "no master was started");

    // The persist time above is five seconds. Twenty is room for a loaded
    // machine, not a different expectation.
    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    while std::time::Instant::now() < deadline {
        if sockets(&directory).is_empty() {
            return;
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    close_master(&fixture, &directory);
    panic!("the master was still there twenty seconds after a five second ControlPersist");
}

/// Without reuse, the same four operations cost four authentications.
///
/// The control: it is what makes the number above mean something, and it is
/// the behaviour every Clift before this one had.
#[test]
fn without_reuse_each_operation_authenticates_for_itself() {
    if skip_without_docker("without_reuse_each_operation_authenticates_for_itself") {
        return;
    }
    let fixture = SshdFixture::start(Topology::Normal);
    let target = TransportTarget::new(fixture.alias());
    let before = accepted_connections(&fixture);

    let transport =
        OpenSshTransport::with_runner(SshRunner::new().with_config_file(fixture.ssh_config()));
    for index in 0..4 {
        transport
            .ensure_dir(&target, &home(&fixture, &format!("plain-{index}")), 0o700)
            .unwrap();
    }

    // One `ensure_dir` is several SFTP operations, and without a master each
    // of them is its own connection -- which is the point. The assertion is on
    // the shape of the number rather than its exact value: what matters is
    // that it grows with the work, where the reused one does not.
    let authentications = accepted_connections(&fixture) - before;
    assert!(
        authentications >= 4,
        "four operations authenticated {authentications} times; without reuse each one \
         pays for itself, so this cannot be fewer than four"
    );
}

/// A host the user already multiplexes keeps the user's settings.
///
/// Checked by looking at which socket exists afterwards. Clift's directory
/// must be empty and the user's must not be: an option on Clift's command line
/// would have won over their configuration file, and this is what proves it
/// was never passed.
#[test]
fn a_user_who_already_multiplexes_keeps_their_own_socket() {
    if skip_without_docker("a_user_who_already_multiplexes_keeps_their_own_socket") {
        return;
    }
    let fixture = SshdFixture::start(Topology::Normal);
    let theirs = socket_dir("t");
    let ours = socket_dir("o");

    let config = fixture.variant_config("multiplexed", |text| {
        format!(
            "{text}\nHost *\n    ControlMaster auto\n    ControlPath {}/%C\n    ControlPersist 5\n",
            theirs.display()
        )
    });

    let transport = OpenSshTransport::with_runner(
        SshRunner::new()
            .with_config_file(&config)
            .with_reuse(reuse(&ours)),
    );
    transport
        .ensure_dir(
            &TransportTarget::new(fixture.alias()),
            &home(&fixture, "theirs"),
            0o700,
        )
        .unwrap();

    assert_eq!(
        sockets(&ours),
        Vec::<String>::new(),
        "Clift opened a master of its own on a host the user already multiplexes"
    );
    assert_eq!(
        sockets(&theirs).len(),
        1,
        "the user's own multiplexing stopped working: {:?}",
        sockets(&theirs)
    );

    let mut control = std::ffi::OsString::from("ControlPath=");
    control.push(theirs.join("%C"));
    let _ = Command::new("ssh")
        .arg("-F")
        .arg(&config)
        .arg("-o")
        .arg(control)
        .arg("-O")
        .arg("exit")
        .arg(fixture.alias())
        .output();
}
