//! Requests a key press sends together, against a real server.
//!
//! On a distant host a send takes its round trips multiplied by the round trip
//! time, so the number of times the client waits for the server is the number
//! worth pinning. A local container cannot show it: its round trip is under a
//! millisecond. Here every byte the server sends is held back for [`DELAY`] by
//! a proxy the test runs, so an operation's elapsed time divided by that delay
//! is how many times it waited.
//!
//! Sending requests together must not change what they decide. The one outcome
//! it could plausibly change is tested here too: a batch directory created
//! alongside an inbox that then fails its check is not left behind.

#![allow(clippy::unwrap_used, clippy::expect_used)]

#[path = "../../../tests/e2e/fixtures.rs"]
mod fixtures;

use clift_core::domain::{
    BATCH_ID_BYTES, BatchId, FileKind, LocalAttachment, RemotePath, SafeFileName,
};
use clift_core::error::{CliftError, ErrorKind, Stage};
use clift_core::ports::{Clock, IdSource, TransportTarget};
use clift_core::usecase::{SendPolicy, perform};
use clift_transport::probe::OpenSshTransport;
use clift_transport::proc::SshRunner;
use fixtures::{SshdFixture, Topology, skip_without_docker};
use std::io::{Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::process::Command;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant, SystemTime};

/// How long every byte from the server is held back: long enough that the few
/// milliseconds of local work in an operation cannot add up to another one.
const DELAY: Duration = Duration::from_millis(300);

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

/// Relays each connection to the fixture, holding back what the server sends.
///
/// Every chunk keeps the moment it arrived and is released [`DELAY`] after
/// it, so replies the server sent together still arrive together: the proxy
/// adds a round trip time, not a wait per packet.
fn delaying_proxy(port: u16) -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let local = listener.local_addr().unwrap().port();
    thread::spawn(move || {
        for client in listener.incoming().flatten() {
            if let Ok(server) = TcpStream::connect(("127.0.0.1", port)) {
                relay(client, server);
            }
        }
    });
    local
}

fn relay(client: TcpStream, server: TcpStream) {
    let mut upstream_in = client.try_clone().unwrap();
    let mut upstream_out = server.try_clone().unwrap();
    thread::spawn(move || {
        let _ = std::io::copy(&mut upstream_in, &mut upstream_out);
        let _ = upstream_out.shutdown(Shutdown::Write);
    });

    let (chunks, delivered) = mpsc::channel::<(Instant, Vec<u8>)>();
    let mut downstream_in = server;
    thread::spawn(move || {
        let mut buffer = [0_u8; 65_536];
        while let Ok(read) = downstream_in.read(&mut buffer) {
            if read == 0
                || chunks
                    .send((Instant::now(), buffer[..read].to_vec()))
                    .is_err()
            {
                break;
            }
        }
    });
    let mut downstream_out = client;
    thread::spawn(move || {
        for (arrived, bytes) in delivered {
            let due = arrived + DELAY;
            let now = Instant::now();
            if due > now {
                thread::sleep(due - now);
            }
            if downstream_out.write_all(&bytes).is_err() {
                break;
            }
        }
        let _ = downstream_out.shutdown(Shutdown::Write);
    });
}

/// A transport whose connections to the fixture go through [`delaying_proxy`],
/// keeping one SFTP session as the composition root does.
///
/// The proxy is reached with `ProxyCommand`, so `ssh` still takes itself to be
/// talking to the fixture's own address and port, and checks the host key it
/// pinned for them.
fn delayed(fixture: &SshdFixture) -> OpenSshTransport {
    let located = Command::new("/usr/bin/which")
        .arg("nc")
        .output()
        .expect("which must be runnable");
    let nc = String::from_utf8_lossy(&located.stdout).trim().to_string();
    assert!(!nc.is_empty(), "these tests need nc to reach their proxy");

    let port = delaying_proxy(fixture.port().parse().unwrap());
    let host = format!("Host {}\n", fixture.alias());
    let config = fixture.variant_config("delayed", |text| {
        assert!(text.contains(&host), "unexpected fixture config:\n{text}");
        text.replacen(
            &host,
            &format!("{host}    ProxyCommand {nc} 127.0.0.1 {port}\n"),
            1,
        )
    });
    OpenSshTransport::with_runner(SshRunner::new().with_config_file(config).with_sessions())
}

fn round_trips(elapsed: Duration) -> u128 {
    elapsed.as_millis() / DELAY.as_millis()
}

fn home(fixture: &SshdFixture, suffix: &str) -> RemotePath {
    RemotePath::new(format!("{}/{suffix}", fixture.remote_home())).unwrap()
}

fn name(value: &str) -> SafeFileName {
    SafeFileName::new(value).unwrap()
}

fn remote(fixture: &SshdFixture, command: &str) -> String {
    let output = fixture.ssh(command);
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

/// An upload into a session that is already open waits for the server three
/// times: for the handle; for the writes, sent with the read-back and the
/// close; and for the rename, which only happens once the read-back has been
/// checked.
#[test]
fn an_upload_costs_three_round_trips() {
    if skip_without_docker("an_upload_costs_three_round_trips") {
        return;
    }
    let fixture = SshdFixture::start(Topology::Normal);
    let target = TransportTarget::new(fixture.alias());
    let transport = delayed(&fixture);
    let directory = home(&fixture, "pipelined-upload");
    transport.ensure_dir(&target, &directory, 0o700).unwrap();

    let local = fixture.workdir().join("shot.png");
    std::fs::write(&local, vec![7_u8; 100_000]).unwrap();
    let destination = directory.join(&name("shot.png"));

    let started = Instant::now();
    let sent = transport
        .upload_atomic(&target, &local, &destination)
        .unwrap();
    let elapsed = started.elapsed();

    assert_eq!(sent, 100_000);
    assert_eq!(round_trips(elapsed), 3, "the upload took {elapsed:?}");
    assert_eq!(
        remote(&fixture, &format!("stat -c %a '{destination}'")),
        "600"
    );
}

/// The inbox check and a new batch directory inside it cost one round trip
/// together.
#[test]
fn the_inbox_and_a_new_batch_directory_cost_one_round_trip() {
    if skip_without_docker("the_inbox_and_a_new_batch_directory_cost_one_round_trip") {
        return;
    }
    let fixture = SshdFixture::start(Topology::Normal);
    let target = TransportTarget::new(fixture.alias());
    let transport = delayed(&fixture);
    let root = home(&fixture, "pipelined-inbox");
    let date = root.join(&name("2026-09-14"));
    transport.ensure_dir(&target, &date, 0o700).unwrap();
    let batch = date.join(&name("0123456789abcdef"));

    let started = Instant::now();
    transport
        .ensure_dirs(&target, &[&root, &batch], 0o700)
        .unwrap();
    let elapsed = started.elapsed();

    assert_eq!(
        round_trips(elapsed),
        1,
        "the two directories took {elapsed:?}"
    );
    assert_eq!(remote(&fixture, &format!("stat -c %a '{batch}'")), "700");
}

/// A second send, to a host whose home and cache directory are recorded and
/// whose session is open, waits for the server four times: once for the inbox
/// and the batch directory, three times for the upload.
#[test]
fn a_second_send_costs_four_round_trips() {
    if skip_without_docker("a_second_send_costs_four_round_trips") {
        return;
    }
    let fixture = SshdFixture::start(Topology::Normal);
    let target = TransportTarget::new(fixture.alias());
    let transport = delayed(&fixture);
    let known_home = RemotePath::new(fixture.remote_home()).unwrap();
    let known_cache = home(&fixture, ".cache");
    let policy = SendPolicy {
        remote_home: Some(&known_home),
        remote_cache_home: Some(&known_cache),
        ..SendPolicy::default()
    };

    let local = fixture.workdir().join("shot.png");
    std::fs::write(&local, vec![7_u8; 100_000]).unwrap();
    let attachment =
        LocalAttachment::new(local, name("shot.png"), 100_000, FileKind::Regular).unwrap();

    // The first send opens the session and creates the inbox and today's
    // directory, which is what a second press finds already there.
    perform(
        &transport,
        &target,
        std::slice::from_ref(&attachment),
        &policy,
        &SystemClock,
        &SystemIdSource,
    )
    .unwrap();

    let started = Instant::now();
    let outcome = perform(
        &transport,
        &target,
        std::slice::from_ref(&attachment),
        &policy,
        &SystemClock,
        &SystemIdSource,
    )
    .unwrap();
    let elapsed = started.elapsed();

    assert!(outcome.insertion_text().contains("/.cache/clift/inbox/"));
    assert_eq!(round_trips(elapsed), 4, "the second send took {elapsed:?}");
}

/// A batch directory asked for alongside an inbox that fails its check is
/// removed again, not left inside it.
///
/// The inbox is readable by everyone, the way a user's own `mkdir` would leave
/// it, and today's directory already exists inside it, so the batch's own
/// `mkdir` succeeds before the inbox's answer has been read.
#[test]
fn a_batch_asked_for_with_an_inbox_that_fails_its_check_is_not_left_behind() {
    if skip_without_docker(
        "a_batch_asked_for_with_an_inbox_that_fails_its_check_is_not_left_behind",
    ) {
        return;
    }
    let fixture = SshdFixture::start(Topology::Normal);
    let target = TransportTarget::new(fixture.alias());
    let transport = OpenSshTransport::with_runner(
        SshRunner::new()
            .with_config_file(fixture.ssh_config())
            .with_sessions(),
    );
    let root = home(&fixture, "loose-inbox");
    let date = root.join(&name("2026-09-14"));
    let prepared = fixture.ssh(&format!("mkdir -p '{date}' && chmod 755 '{root}' '{date}'"));
    assert!(
        prepared.status.success(),
        "{}",
        String::from_utf8_lossy(&prepared.stderr)
    );
    let batch = date.join(&name("0123456789abcdef"));

    let error = transport
        .ensure_dirs(&target, &[&root, &batch], 0o700)
        .expect_err("an inbox readable by everyone must be refused");

    assert_eq!(error.exit_code().as_u8(), 25, "{error}");
    assert_eq!(
        remote(
            &fixture,
            &format!("test -e '{batch}' && echo left || echo gone")
        ),
        "gone",
        "the batch directory was left inside the refused inbox"
    );
    assert_eq!(remote(&fixture, &format!("stat -c %a '{root}'")), "755");
}
