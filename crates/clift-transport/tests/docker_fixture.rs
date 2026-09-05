//! The throwaway OpenSSH fixture the transport tests are built on.
//!
//! These tests check the fixture itself. If they fail, every later transport
//! test is testing nothing, so they are deliberately explicit about what the
//! container is expected to be: a non-root account, a writable home, a working
//! SFTP subsystem, and host key verification that is genuinely on.

#![allow(clippy::unwrap_used)]

#[path = "../../../tests/e2e/fixtures.rs"]
mod fixtures;

use fixtures::{SshdFixture, Topology, container_is_gone, skip_without_docker};

#[test]
fn a_normal_fixture_accepts_ssh_and_sftp_as_a_non_root_user() {
    if skip_without_docker("a_normal_fixture_accepts_ssh_and_sftp_as_a_non_root_user") {
        return;
    }
    let fixture = SshdFixture::start(Topology::Normal);

    let whoami = fixture.ssh("id -un; echo \"$HOME\"");
    assert!(
        whoami.status.success(),
        "ssh failed: {}",
        String::from_utf8_lossy(&whoami.stderr)
    );
    let stdout = String::from_utf8_lossy(&whoami.stdout).into_owned();
    let mut lines = stdout.lines();
    assert_eq!(
        lines.next(),
        Some("dev"),
        "the fixture must log in as a plain account; root would mask permission bugs"
    );
    assert_eq!(lines.next(), Some(fixture.remote_home()));

    let write = fixture.ssh("touch \"$HOME/writable\" && echo ok");
    assert!(
        write.status.success(),
        "the home must be writable: {}",
        String::from_utf8_lossy(&write.stderr)
    );

    let sftp = fixture.sftp("pwd\nquit\n");
    assert!(
        sftp.status.success(),
        "sftp failed: {}",
        String::from_utf8_lossy(&sftp.stderr)
    );
    assert!(
        String::from_utf8_lossy(&sftp.stdout).contains(fixture.remote_home()),
        "sftp did not report the remote home: {}",
        String::from_utf8_lossy(&sftp.stdout)
    );
}

#[test]
fn host_key_verification_is_enforced_not_bypassed() {
    if skip_without_docker("host_key_verification_is_enforced_not_bypassed") {
        return;
    }
    let fixture = SshdFixture::start(Topology::Normal);

    // Same server, same client key, same everything except that the pinned host
    // key is taken away. If the fixture were reaching the container by relaxing
    // a verification setting, this would still connect.
    let empty_known_hosts = fixture.workdir().join("known_hosts.empty");
    std::fs::write(&empty_known_hosts, "").unwrap();
    let config = std::fs::read_to_string(fixture.ssh_config()).unwrap();
    let unpinned_config = fixture.workdir().join("ssh_config.unpinned");
    let known_hosts = fixture.workdir().join("known_hosts");
    std::fs::write(
        &unpinned_config,
        config.replace(
            known_hosts.to_str().unwrap(),
            empty_known_hosts.to_str().unwrap(),
        ),
    )
    .unwrap();

    let output = std::process::Command::new("ssh")
        .arg("-F")
        .arg(&unpinned_config)
        .arg(fixture.alias())
        .arg("true")
        .output()
        .unwrap();
    assert!(
        !output.status.success(),
        "an unknown host key must not be accepted"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Host key verification failed"),
        "expected a host key rejection, got: {stderr}"
    );
}

#[test]
fn the_no_sftp_topology_fails_the_subsystem_and_only_the_subsystem() {
    if skip_without_docker("the_no_sftp_topology_fails_the_subsystem_and_only_the_subsystem") {
        return;
    }
    let fixture = SshdFixture::start(Topology::NoSftp);

    let login = fixture.ssh("echo reachable");
    assert!(
        login.status.success(),
        "the host must still be reachable, otherwise the test cannot tell a \
         missing subsystem apart from a broken connection: {}",
        String::from_utf8_lossy(&login.stderr)
    );

    let sftp = fixture.sftp("pwd\nquit\n");
    assert!(!sftp.status.success(), "sftp must not succeed");
    let stderr = String::from_utf8_lossy(&sftp.stderr);
    assert!(
        stderr.contains("subsystem request failed"),
        "expected a subsystem failure, got: {stderr}"
    );
}

#[test]
fn the_readonly_home_topology_refuses_writes_after_a_successful_login() {
    if skip_without_docker("the_readonly_home_topology_refuses_writes_after_a_successful_login") {
        return;
    }
    let fixture = SshdFixture::start(Topology::ReadonlyHome);

    let login = fixture.ssh("echo reachable");
    assert!(
        login.status.success(),
        "login must succeed so that the failure lands on the write: {}",
        String::from_utf8_lossy(&login.stderr)
    );

    let sftp = fixture.sftp(&format!("mkdir {}/inbox\nquit\n", fixture.remote_home()));
    assert!(
        !sftp.status.success(),
        "creating a directory in a read-only home must fail"
    );
    let stderr = String::from_utf8_lossy(&sftp.stderr);
    assert!(
        stderr.to_lowercase().contains("permission denied"),
        "expected a permission failure, got: {stderr}"
    );
}

#[test]
fn the_small_quota_topology_runs_out_of_space() {
    if skip_without_docker("the_small_quota_topology_runs_out_of_space") {
        return;
    }
    let fixture = SshdFixture::start(Topology::SmallQuota);

    let quota = format!("{}/quota", fixture.remote_home());
    let filled = fixture.ssh(&format!(
        "dd if=/dev/zero of={quota}/big bs=1M count=4 2>&1 || true"
    ));
    let report = String::from_utf8_lossy(&filled.stdout).to_lowercase();
    assert!(
        report.contains("no space left on device"),
        "the size-limited filesystem did not fill up: {report}"
    );
}

#[test]
fn a_dropped_fixture_leaves_no_container_behind() {
    if skip_without_docker("a_dropped_fixture_leaves_no_container_behind") {
        return;
    }
    let container = {
        let fixture = SshdFixture::start(Topology::Normal);
        assert!(
            !container_is_gone(fixture.container()),
            "the container should exist while the fixture is alive"
        );
        fixture.container().to_string()
    };
    assert!(
        container_is_gone(&container),
        "container {container} outlived its fixture"
    );
}
