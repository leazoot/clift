//! The subprocess base, exercised against a real OpenSSH server.
//!
//! The unit tests in `proc.rs` pin down which arguments Clift generates. These
//! check the other half: that awkward paths reach a real server unchanged, and
//! that a hung client is stopped rather than waited on forever.

#![allow(clippy::unwrap_used)]

#[path = "../../../tests/e2e/fixtures.rs"]
mod fixtures;

use clift_core::domain::RemotePath;
use clift_core::ports::TransportTarget;
use clift_transport::probe::OpenSshTransport;
use clift_transport::proc::SshRunner;
use fixtures::{SshdFixture, Topology, skip_without_docker};
use std::time::{Duration, Instant};

fn runner(fixture: &SshdFixture) -> SshRunner {
    SshRunner::new().with_config_file(fixture.ssh_config())
}

fn target(fixture: &SshdFixture) -> TransportTarget {
    TransportTarget::new(fixture.alias())
}

#[test]
fn a_literal_remote_command_runs_and_its_output_comes_back() {
    if skip_without_docker("a_literal_remote_command_runs_and_its_output_comes_back") {
        return;
    }
    let fixture = SshdFixture::start(Topology::Normal);

    let outcome = runner(&fixture)
        .run_ssh(&target(&fixture), "id -un")
        .unwrap();
    assert!(outcome.succeeded(), "{outcome:?}");
    assert_eq!(outcome.stdout.trim(), "dev");
    assert_eq!(outcome.stderr, "");
}

#[test]
fn a_non_zero_exit_is_reported_as_an_outcome_not_as_an_error() {
    if skip_without_docker("a_non_zero_exit_is_reported_as_an_outcome_not_as_an_error") {
        return;
    }
    let fixture = SshdFixture::start(Topology::Normal);

    // Deciding what a non-zero status means belongs to the caller: a probe
    // reads it as "this check failed", not as "the transport broke".
    let outcome = runner(&fixture)
        .run_ssh(&target(&fixture), "false")
        .unwrap();
    assert!(!outcome.succeeded());
    assert_eq!(outcome.code, Some(1));
}

#[test]
fn paths_with_spaces_and_non_ascii_survive_the_round_trip() {
    if skip_without_docker("paths_with_spaces_and_non_ascii_survive_the_round_trip") {
        return;
    }
    let fixture = SshdFixture::start(Topology::Normal);
    let transport = OpenSshTransport::with_runner(runner(&fixture));

    let directory = RemotePath::new(format!("{}/截图 目录 2", fixture.remote_home())).unwrap();
    transport
        .ensure_dir(&target(&fixture), &directory, 0o700)
        .unwrap();

    let listing = fixture.ssh("ls -1 \"$HOME\"");
    assert_eq!(
        String::from_utf8_lossy(&listing.stdout).trim(),
        "截图 目录 2",
        "the directory name did not survive"
    );
    let mode = fixture.ssh("stat -c %a \"$HOME/截图 目录 2\"");
    assert_eq!(String::from_utf8_lossy(&mode.stdout).trim(), "700");
}

#[test]
fn quotes_backslashes_and_glob_characters_are_carried_literally() {
    if skip_without_docker("quotes_backslashes_and_glob_characters_are_carried_literally") {
        return;
    }
    let fixture = SshdFixture::start(Topology::Normal);
    let transport = OpenSshTransport::with_runner(runner(&fixture));
    let target = target(&fixture);

    // A name that would be mangled by a remote shell, by a client-side
    // tokeniser, or by glob expansion, depending on which one is careless.
    let awkward = RemotePath::new(format!(
        "{}/it's \"a\" back\\slash *?[x] $HOME `id`",
        fixture.remote_home()
    ))
    .unwrap();
    let decoy = RemotePath::new(format!(
        "{}/it's \"a\" back\\slash ZZZZZZ $HOME `id`",
        fixture.remote_home()
    ))
    .unwrap();
    transport.ensure_dir(&target, &awkward, 0o700).unwrap();
    transport.ensure_dir(&target, &decoy, 0o700).unwrap();

    // Removing the globbed name must remove exactly one directory. If the
    // metacharacters were expanded anywhere, the decoy would go too.
    transport.remove(&target, &awkward).unwrap();

    let listing = fixture.ssh("ls -1 \"$HOME\"");
    let listing = String::from_utf8_lossy(&listing.stdout);
    assert!(
        listing.contains("ZZZZZZ"),
        "the decoy was removed as well; the path was expanded: {listing}"
    );
    assert!(
        !listing.contains("*?[x]"),
        "the awkward directory was not removed: {listing}"
    );
}

#[test]
fn a_hung_client_is_stopped_at_the_timeout_and_says_so() {
    if skip_without_docker("a_hung_client_is_stopped_at_the_timeout_and_says_so") {
        return;
    }
    let fixture = SshdFixture::start(Topology::Normal);
    let runner = runner(&fixture).with_timeout(Duration::from_secs(1));

    let started = Instant::now();
    let error = runner
        .run_ssh(&target(&fixture), "sleep 30")
        .expect_err("a command that outlasts the timeout must fail");
    let elapsed = started.elapsed();

    assert!(
        elapsed < Duration::from_secs(10),
        "the client was not stopped promptly: {elapsed:?}"
    );
    assert_eq!(error.exit_code().as_u8(), 22);
    assert!(
        error.message().contains("did not finish within 1 seconds"),
        "{error}"
    );
    assert!(
        error.remedy().is_some(),
        "a timeout must come with something the user can run"
    );
}
