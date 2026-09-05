//! `clift setup` end to end, against a real OpenSSH server.
//!
//! The interesting claims are not "it works" but the three things that must
//! hold when it does not: no half-written configuration, no leftover test file
//! on the host, and no change to the user's own SSH configuration.

#![allow(clippy::unwrap_used, clippy::expect_used)]

#[path = "../../../tests/e2e/fixtures.rs"]
mod fixtures;

use fixtures::{SshdFixture, Topology, skip_without_docker};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// A home directory of its own, plus `ssh` and `sftp` shims on `PATH` that
/// point the real clients at the fixture's configuration.
///
/// `HOME` is not enough on its own: OpenSSH resolves `~` through the password
/// database rather than the environment, so a test cannot move `~/.ssh/config`
/// that way. Shimming `PATH` is how a user would substitute a client anyway,
/// and it leaves the production code with no test-only entry point -- there is
/// no environment variable that redirects Clift's SSH configuration, and there
/// must not be one.
struct Sandbox {
    home: PathBuf,
    bin: PathBuf,
}

impl Sandbox {
    fn new(fixture: &SshdFixture, label: &str) -> Self {
        let home = fixture.workdir().join(format!("home-{label}"));
        let bin = fixture.workdir().join(format!("bin-{label}"));
        fs::create_dir_all(home.join(".ssh")).unwrap();
        fs::create_dir_all(&bin).unwrap();

        // A copy the test can compare against afterwards: setup must not touch
        // the user's own SSH configuration.
        let config = fs::read_to_string(fixture.ssh_config()).unwrap();
        fs::write(home.join(".ssh").join("config"), config).unwrap();

        for client in ["ssh", "sftp"] {
            let real = which(client);
            let shim = bin.join(client);
            fs::write(
                &shim,
                format!(
                    "#!/bin/sh\nexec {} -F {} \"$@\"\n",
                    real.display(),
                    fixture.ssh_config().display()
                ),
            )
            .unwrap();
            make_executable(&shim);
        }
        Self { home, bin }
    }

    fn ssh_config(&self) -> PathBuf {
        self.home.join(".ssh").join("config")
    }

    fn config_file(&self) -> PathBuf {
        self.home.join(".config").join("clift").join("config.toml")
    }

    fn run(&self, args: &[&str]) -> Output {
        let path = format!(
            "{}:{}",
            self.bin.display(),
            std::env::var("PATH").unwrap_or_default()
        );
        Command::new(env!("CARGO_BIN_EXE_clift"))
            .args(args)
            .env("HOME", &self.home)
            .env("PATH", path)
            .env_remove("XDG_CONFIG_HOME")
            .env_remove("XDG_CACHE_HOME")
            .env_remove("XDG_RUNTIME_DIR")
            .env("NO_COLOR", "1")
            .output()
            .expect("the clift binary must be runnable")
    }
}

fn which(program: &str) -> PathBuf {
    let output = Command::new("/usr/bin/which")
        .arg(program)
        .output()
        .expect("which must be runnable");
    PathBuf::from(String::from_utf8_lossy(&output.stdout).trim())
}

#[cfg(unix)]
fn make_executable(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
}

fn stderr_of(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn remote_mode(fixture: &SshdFixture, path: &str) -> String {
    let output = fixture.ssh(&format!("stat -c %a \"{path}\""));
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

#[cfg(unix)]
fn local_mode(path: &Path) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    fs::metadata(path).unwrap().permissions().mode() & 0o777
}

/// Acceptance 1, 3, 4 and 5 in one run: the four ticks, the test file cleaned
/// up, the user's SSH configuration untouched, and no key path in the output.
#[test]
fn a_successful_setup_prints_four_ticks_and_leaves_nothing_behind() {
    if skip_without_docker("a_successful_setup_prints_four_ticks_and_leaves_nothing_behind") {
        return;
    }
    let fixture = SshdFixture::start(Topology::Normal);
    let home = Sandbox::new(&fixture, "ok");
    let before = fs::read(home.ssh_config()).unwrap();

    let output = home.run(&["setup", fixture.alias(), "--yes"]);
    let stderr = stderr_of(&output);
    assert!(output.status.success(), "setup failed: {stderr}");

    // stdout is for machine results only, and this run produced none.
    assert!(
        output.stdout.is_empty(),
        "human output reached stdout: {:?}",
        String::from_utf8_lossy(&output.stdout)
    );

    // Progress is drawn only on a terminal. Captured like this there must be
    // no escape sequence and no animation frame, so a redirected run stays
    // diffable and a log file stays readable.
    assert!(
        !stderr.contains('\u{1b}'),
        "an escape sequence reached a captured stderr:\n{stderr:?}"
    );
    for frame in ["\u{280b}", "\u{2819}", "\u{2839}"] {
        assert!(
            !stderr.contains(frame),
            "a spinner frame was drawn without a terminal:\n{stderr:?}"
        );
    }

    for line in [
        "\u{2713} SSH connection",
        "\u{2713} SFTP subsystem",
        "\u{2713} Private inbox",
        "\u{2713} Upload and cleanup",
        "Ready. Run: clift send --clipboard --to",
    ] {
        assert!(stderr.contains(line), "missing {line:?} in:\n{stderr}");
    }

    // The configuration is written, private, and describes the host.
    let config = home.config_file();
    let text = fs::read_to_string(&config).unwrap();
    assert!(
        text.contains(&format!("[targets.{}]", fixture.alias())),
        "{text}"
    );
    assert!(text.contains("remote_home"), "{text}");
    assert!(text.contains("last_success_at"), "{text}");
    #[cfg(unix)]
    assert_eq!(local_mode(&config), 0o600, "the config must be private");

    // The inbox exists and is private; the self-check file does not.
    let inbox = format!("{}/.cache/clift/inbox", fixture.remote_home());
    assert_eq!(remote_mode(&fixture, &inbox), "700");
    let listing = fixture.ssh(&format!("ls -A \"{inbox}\""));
    assert!(
        String::from_utf8_lossy(&listing.stdout).trim().is_empty(),
        "the self check left something behind: {}",
        String::from_utf8_lossy(&listing.stdout)
    );

    // The user's own SSH configuration is not Clift's to edit.
    assert_eq!(
        fs::read(home.ssh_config()).unwrap(),
        before,
        "setup modified ~/.ssh/config"
    );

    for forbidden in ["identityfile", "IdentityFile", "id_ed25519", "id_rsa"] {
        assert!(
            !stderr.contains(forbidden),
            "a key location reached the output: {stderr}"
        );
    }
}

/// Acceptance 2: a host that fails a check leaves no configuration at all, not
/// even a partial one.
#[test]
fn a_failed_setup_writes_no_configuration() {
    if skip_without_docker("a_failed_setup_writes_no_configuration") {
        return;
    }
    let fixture = SshdFixture::start(Topology::NoSftp);
    let home = Sandbox::new(&fixture, "no-sftp");

    let output = home.run(&["setup", fixture.alias(), "--yes"]);
    assert!(!output.status.success(), "the host has no SFTP subsystem");
    assert_eq!(
        output.status.code(),
        Some(22),
        "an SFTP subsystem that is missing is a connection failure: {}",
        stderr_of(&output)
    );
    assert!(
        output.stdout.is_empty(),
        "a failure must leave stdout empty"
    );
    assert!(
        !home.config_file().exists(),
        "a failed setup wrote a configuration file"
    );
}

/// With nobody to answer, the run fails rather than waiting.
/// The test harness gives the child a pipe rather than a terminal, which is
/// exactly the situation being tested.
#[test]
fn a_non_interactive_run_without_yes_fails_instead_of_waiting() {
    if skip_without_docker("a_non_interactive_run_without_yes_fails_instead_of_waiting") {
        return;
    }
    let fixture = SshdFixture::start(Topology::Normal);
    let home = Sandbox::new(&fixture, "no-tty");

    let output = home.run(&["setup", fixture.alias()]);
    assert_eq!(output.status.code(), Some(20), "{}", stderr_of(&output));
    assert!(
        stderr_of(&output).contains("--yes"),
        "{}",
        stderr_of(&output)
    );
    assert!(output.stdout.is_empty());
    assert!(
        !home.config_file().exists(),
        "a cancelled setup wrote a configuration file"
    );
}

/// `--json` puts exactly one document on stdout and nothing else.
#[test]
fn the_json_form_is_one_document_and_no_other_bytes() {
    if skip_without_docker("the_json_form_is_one_document_and_no_other_bytes") {
        return;
    }
    let fixture = SshdFixture::start(Topology::Normal);
    let home = Sandbox::new(&fixture, "json");

    let output = home.run(&["--json", "setup", fixture.alias(), "--yes"]);
    assert!(output.status.success(), "{}", stderr_of(&output));

    let text = String::from_utf8(output.stdout).expect("stdout must be UTF-8");
    let value: serde_json::Value =
        serde_json::from_str(&text).expect("stdout must be one JSON doc");
    assert_eq!(value["schema_version"], 1);
    assert_eq!(value["status"], "ok");
    assert_eq!(value["target"], fixture.alias());
    assert_eq!(value["checks"].as_array().unwrap().len(), 4);
    assert!(
        value["storage"]
            .as_str()
            .unwrap()
            .ends_with("/.cache/clift/inbox"),
        "{value}"
    );
    assert!(
        !text.contains("identityfile") && !text.contains("id_ed25519"),
        "a key location reached the machine output: {text}"
    );
}
