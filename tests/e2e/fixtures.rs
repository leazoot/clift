//! Shared helpers for tests that need a real OpenSSH server.
//!
//! The specification requires the transport layer to be exercised against a real SFTP
//! server, and 23.4 forbids presenting a mocked transport as an end-to-end
//! test. Everything here therefore drives an actual container over an actual
//! SSH connection; there is no in-process substitute to fall back to.
//!
//! Include it from a test binary with:
//! `#[path = "../../../tests/e2e/fixtures.rs"] mod fixtures;`

// The module is shared by several test binaries, each of which uses a subset of
// it. A panic in fixture code is a broken fixture, and reporting it as one is
// the right outcome: swallowing it would let a later test claim to have proved
// something against a server that never started.
#![allow(dead_code, clippy::unwrap_used, clippy::expect_used)]

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};

/// Which shape of server to start.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Topology {
    /// A plain, healthy host: non-root account, writable home, SFTP available.
    Normal,
    /// The SFTP subsystem is missing, which must be reported as such rather
    /// than as a generic connection failure.
    NoSftp,
    /// The home directory is not writable by the account that logs in.
    ReadonlyHome,
    /// The home contains a 1 MiB filesystem, for out-of-space behaviour.
    SmallQuota,
    /// The cache directory -- and so Clift's inbox -- on a 1 MiB filesystem.
    SmallCache,
}

impl Topology {
    const fn as_arg(self) -> &'static str {
        match self {
            Topology::Normal => "normal",
            Topology::NoSftp => "no-sftp",
            Topology::ReadonlyHome => "readonly-home",
            Topology::SmallQuota => "small-quota",
            Topology::SmallCache => "small-cache",
        }
    }
}

/// Writes straight to the process's stderr, bypassing the test harness's output
/// capture.
///
/// A skipped test that only whispers into a captured buffer is indistinguishable
/// from a passing one, which is precisely the silent pass the test plan forbids.
fn announce(line: &str) {
    let mut stderr = std::io::stderr();
    let _ = writeln!(stderr, "{line}");
    let _ = stderr.flush();
}

/// The repository root, found by walking up from the crate being compiled.
fn repo_root() -> PathBuf {
    let mut dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    loop {
        if dir.join("tests/e2e/docker/sshd-fixture.sh").is_file() {
            return dir;
        }
        assert!(
            dir.pop(),
            "could not locate tests/e2e/docker/sshd-fixture.sh above {}",
            env!("CARGO_MANIFEST_DIR")
        );
    }
}

fn fixture_script() -> PathBuf {
    repo_root().join("tests/e2e/docker/sshd-fixture.sh")
}

/// Why Docker cannot be used here, or `None` when it can.
pub fn docker_unavailable_reason() -> Option<String> {
    let output = Command::new(fixture_script()).arg("available").output();
    match output {
        Ok(output) if output.status.success() => None,
        Ok(output) => Some(String::from_utf8_lossy(&output.stderr).trim().to_string()),
        Err(error) => Some(format!("could not run the fixture script: {error}")),
    }
}

/// Reports a skip loudly, or turns it into a failure when the environment says
/// Docker must be present.
///
/// `CLIFT_E2E_REQUIRE_DOCKER` is how a CI job states that a skip there is a
/// broken job rather than an acceptable outcome.
#[must_use]
pub fn skip_without_docker(test_name: &str) -> bool {
    match docker_unavailable_reason() {
        None => false,
        Some(reason) => {
            assert!(
                std::env::var_os("CLIFT_E2E_REQUIRE_DOCKER").is_none(),
                "CLIFT_E2E_REQUIRE_DOCKER is set but Docker is unusable: {reason}"
            );
            announce(&format!(
                "SKIPPED {test_name}: docker is unavailable ({reason}); \
                 this test proved nothing"
            ));
            true
        }
    }
}

/// A running throwaway SSH server, removed when this value is dropped.
pub struct SshdFixture {
    workdir: PathBuf,
    container: String,
    ssh_config: PathBuf,
    alias: String,
    remote_home: String,
    port: String,
}

static FIXTURE_SEQUENCE: AtomicU32 = AtomicU32::new(0);

impl SshdFixture {
    /// Starts a container and waits until it accepts an authenticated
    /// connection.
    ///
    /// # Panics
    /// Panics if the container cannot be started, which is a broken fixture
    /// rather than a product failure and must not be swallowed.
    #[must_use]
    pub fn start(topology: Topology) -> Self {
        let sequence = FIXTURE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let workdir = repo_root().join(format!(
            "target/e2e/{}-{}-{}",
            topology.as_arg(),
            std::process::id(),
            sequence
        ));
        let _ = std::fs::remove_dir_all(&workdir);

        let output = Command::new(fixture_script())
            .arg("start")
            .arg(topology.as_arg())
            .arg(&workdir)
            .output()
            .expect("the fixture script must be runnable");
        assert!(
            output.status.success(),
            "starting the {topology:?} fixture failed:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );

        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        let field = |name: &str| -> String {
            stdout
                .lines()
                .find_map(|line| line.strip_prefix(&format!("{name}=")))
                .unwrap_or_else(|| panic!("the fixture script printed no {name}: {stdout}"))
                .to_string()
        };

        Self {
            container: field("container"),
            ssh_config: PathBuf::from(field("ssh_config")),
            alias: field("alias"),
            remote_home: field("remote_home"),
            port: field("port"),
            workdir,
        }
    }

    /// The ssh_config that reaches this server. It pins the container's real
    /// host key; no verification setting is relaxed to make it work.
    #[must_use]
    pub fn ssh_config(&self) -> &Path {
        &self.ssh_config
    }

    #[must_use]
    pub fn alias(&self) -> &str {
        &self.alias
    }

    #[must_use]
    pub fn remote_home(&self) -> &str {
        &self.remote_home
    }

    #[must_use]
    pub fn port(&self) -> &str {
        &self.port
    }

    #[must_use]
    pub fn container(&self) -> &str {
        &self.container
    }

    #[must_use]
    pub fn workdir(&self) -> &Path {
        &self.workdir
    }

    /// Writes a variant of the fixture's ssh_config and returns its path.
    ///
    /// Used to reproduce failures that are about the client's own view of the
    /// server: the wrong key, an unknown host key, a changed host key.
    #[must_use]
    pub fn variant_config(&self, name: &str, edit: impl FnOnce(String) -> String) -> PathBuf {
        let original = std::fs::read_to_string(&self.ssh_config).expect("the config must exist");
        let path = self.workdir.join(format!("ssh_config.{name}"));
        std::fs::write(&path, edit(original)).expect("the variant config must be writable");
        path
    }

    /// A second, unauthorised key pair, generated on demand.
    #[must_use]
    pub fn spare_identity(&self) -> PathBuf {
        let path = self.workdir.join("spare_id_ed25519");
        if !path.exists() {
            let status = Command::new("ssh-keygen")
                .args(["-q", "-t", "ed25519", "-N", "", "-C", "clift-spare", "-f"])
                .arg(&path)
                .status()
                .expect("ssh-keygen must be runnable");
            assert!(status.success(), "ssh-keygen failed");
        }
        path
    }

    /// A known_hosts file that pins some other server's key to this host.
    #[must_use]
    pub fn mismatched_known_hosts(&self) -> PathBuf {
        let key = self.spare_identity().with_extension("pub");
        let public = std::fs::read_to_string(key).expect("the spare public key must exist");
        let mut parts = public.split_whitespace();
        let algorithm = parts.next().unwrap_or_default();
        let material = parts.next().unwrap_or_default();
        let path = self.workdir.join("known_hosts.mismatched");
        std::fs::write(
            &path,
            format!("[127.0.0.1]:{} {algorithm} {material}\n", self.port),
        )
        .expect("the known_hosts variant must be writable");
        path
    }

    /// Runs a command on the server over SSH.
    #[must_use]
    pub fn ssh(&self, remote_command: &str) -> Output {
        Command::new("ssh")
            .arg("-F")
            .arg(&self.ssh_config)
            .arg(&self.alias)
            .arg(remote_command)
            .output()
            .expect("ssh must be runnable")
    }

    /// Runs a batch of sftp commands against the server.
    #[must_use]
    pub fn sftp(&self, script: &str) -> Output {
        let mut child = Command::new("sftp")
            .arg("-F")
            .arg(&self.ssh_config)
            .arg("-b")
            .arg("-")
            .arg(&self.alias)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("sftp must be runnable");
        child
            .stdin
            .take()
            .expect("stdin was piped")
            .write_all(script.as_bytes())
            .expect("the sftp batch must be writable");
        child.wait_with_output().expect("sftp must terminate")
    }
}

impl Drop for SshdFixture {
    fn drop(&mut self) {
        let stopped = Command::new(fixture_script())
            .arg("stop")
            .arg(&self.workdir)
            .status();
        match stopped {
            Ok(status) if status.success() => {}
            other => announce(&format!(
                "WARNING: container {} may still be running ({other:?})",
                self.container
            )),
        }
        let _ = std::fs::remove_dir_all(&self.workdir);
    }
}

/// True when the container no longer exists.
#[must_use]
pub fn container_is_gone(container: &str) -> bool {
    let output = Command::new("docker")
        .args(["ps", "--all", "--quiet", "--filter"])
        .arg(format!("id={container}"))
        .output()
        .expect("docker must be runnable");
    String::from_utf8_lossy(&output.stdout).trim().is_empty()
}
