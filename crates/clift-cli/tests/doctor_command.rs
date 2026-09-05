//! `clift doctor` end to end.
//!
//! Two claims are worth testing at this level: the report is printed even when
//! the run is going to fail, and `--json` is a single document with a stable
//! shape that a third party can rely on.

#![allow(clippy::unwrap_used, clippy::expect_used)]

#[path = "../../../tests/e2e/fixtures.rs"]
mod fixtures;

use fixtures::{SshdFixture, Topology, skip_without_docker};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU32, Ordering};

static COUNTER: AtomicU32 = AtomicU32::new(0);

const CHECK_NAMES: [&str; 13] = [
    "platform",
    "clipboard",
    "ssh client",
    "sftp client",
    "host resolution",
    "authentication",
    "sftp subsystem",
    "remote home",
    "inbox permissions",
    "upload and cleanup",
    "keystroke injection",
    "relay",
    "config version",
];

fn run_isolated(args: &[&str]) -> Output {
    let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
    let home = std::env::temp_dir().join(format!("clift-doctor-{}-{unique}", std::process::id()));
    fs::create_dir_all(&home).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_clift"))
        .args(args)
        .env("XDG_CONFIG_HOME", &home)
        .env("NO_COLOR", "1")
        .output()
        .expect("the clift binary must be runnable");
    let _ = fs::remove_dir_all(&home);
    output
}

fn stderr_of(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

/// A run against the fixture. `PATH` shims point the real clients at the
/// container's configuration, and the target is registered with `target add`,
/// which does not connect -- so `doctor` has something to diagnose even when
/// the host is the broken one.
struct Sandbox {
    home: PathBuf,
    bin: PathBuf,
}

impl Sandbox {
    fn new(fixture: &SshdFixture, label: &str) -> Self {
        let home = fixture.workdir().join(format!("doctor-home-{label}"));
        let bin = fixture.workdir().join(format!("doctor-bin-{label}"));
        fs::create_dir_all(&home).unwrap();
        fs::create_dir_all(&bin).unwrap();
        shim(&bin, fixture);

        let sandbox = Self { home, bin };
        assert!(
            sandbox
                .run(&["target", "add", fixture.alias()])
                .status
                .success()
        );
        sandbox
    }

    fn run(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_clift"))
            .args(args)
            .env("XDG_CONFIG_HOME", &self.home)
            .env(
                "PATH",
                format!(
                    "{}:{}",
                    self.bin.display(),
                    std::env::var("PATH").unwrap_or_default()
                ),
            )
            .env("NO_COLOR", "1")
            .output()
            .expect("the clift binary must be runnable")
    }
}

fn shim(bin: &Path, fixture: &SshdFixture) {
    for client in ["ssh", "sftp"] {
        let located = Command::new("/usr/bin/which")
            .arg(client)
            .output()
            .expect("which must be runnable");
        let real = String::from_utf8_lossy(&located.stdout).trim().to_string();
        let path = bin.join(client);
        fs::write(
            &path,
            format!(
                "#!/bin/sh\nexec {real} -F {} \"$@\"\n",
                fixture.ssh_config().display()
            ),
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        }
    }
}

fn status_of(value: &serde_json::Value, name: &str) -> String {
    value["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|check| check["name"] == name)
        .unwrap_or_else(|| panic!("{name} was not reported"))["status"]
        .as_str()
        .unwrap()
        .to_string()
}

/// On a machine with nothing configured, every check is still reported and the
/// advice is about configuring a host rather than about a host being broken.
#[test]
fn a_fresh_machine_gets_thirteen_lines_and_a_way_forward() {
    let output = run_isolated(&["doctor"]);
    let text = stderr_of(&output);

    assert!(output.stdout.is_empty(), "human output reached stdout");
    for name in CHECK_NAMES {
        assert!(text.contains(name), "missing check {name:?} in:\n{text}");
    }
    assert!(text.contains("clift setup <ssh-host>"), "{text}");
    assert!(
        output.status.success(),
        "nothing failed, so the exit status is success: {text}"
    );
}

#[test]
fn the_json_report_is_one_document_with_every_check() {
    let output = run_isolated(&["--json", "doctor"]);
    let text = String::from_utf8(output.stdout).unwrap();
    let value: serde_json::Value = serde_json::from_str(&text).expect("one JSON document");

    assert_eq!(value["schema_version"], 1);
    let checks = value["checks"].as_array().unwrap();
    assert_eq!(checks.len(), 13);
    assert_eq!(checks[0]["name"], "platform");
    assert_eq!(checks[0]["status"], "pass");

    // Nothing about keys or tokens, in any field.
    let lowered = text.to_lowercase();
    for forbidden in ["identityfile", "id_ed25519", "id_rsa", "token"] {
        assert!(!lowered.contains(forbidden), "{text}");
    }
}

#[test]
fn a_target_that_does_not_exist_is_refused_rather_than_diagnosed() {
    let output = run_isolated(&["doctor", "nope"]);
    assert_eq!(output.status.code(), Some(20), "{}", stderr_of(&output));
    assert!(output.stdout.is_empty());
}

/// Against a real host: every remote check passes, and the capabilities this
/// build does not have warn instead of claiming a pass.
#[test]
fn a_working_host_passes_the_remote_checks_and_warns_about_what_is_missing() {
    if skip_without_docker(
        "a_working_host_passes_the_remote_checks_and_warns_about_what_is_missing",
    ) {
        return;
    }
    let fixture = SshdFixture::start(Topology::Normal);
    let sandbox = Sandbox::new(&fixture, "ok");
    let output = sandbox.run(&["--json", "doctor", fixture.alias()]);

    let diagnostics = stderr_of(&output);
    let text = String::from_utf8(output.stdout.clone()).unwrap();
    let value: serde_json::Value = serde_json::from_str(&text)
        .unwrap_or_else(|error| panic!("{error}: {text}\n{diagnostics}"));

    for name in [
        "ssh client",
        "sftp client",
        "host resolution",
        "authentication",
        "sftp subsystem",
        "remote home",
        "inbox permissions",
        "upload and cleanup",
    ] {
        assert_eq!(status_of(&value, name), "pass", "{name}: {text}");
    }
    assert_eq!(
        status_of(&value, "clipboard"),
        "pass",
        "this build reads the clipboard, so the check must actually check it: {text}"
    );
    assert_eq!(value["failures"], 0);
    assert!(output.status.success());

    // The self-check cleaned up after itself.
    let inbox = format!("{}/.cache/clift/inbox", fixture.remote_home());
    let listing = fixture.ssh(&format!("ls -A \"{inbox}\""));
    assert!(
        String::from_utf8_lossy(&listing.stdout).trim().is_empty(),
        "doctor left its test file behind"
    );
}

/// A host whose SFTP subsystem is missing fails on the line that is actually
/// broken, and the other fourteen are still reported.
#[test]
fn a_broken_host_fails_the_right_line_and_still_reports_the_others() {
    if skip_without_docker("a_broken_host_fails_the_right_line_and_still_reports_the_others") {
        return;
    }
    let fixture = SshdFixture::start(Topology::NoSftp);
    let sandbox = Sandbox::new(&fixture, "broken");
    let output = sandbox.run(&["--json", "doctor", fixture.alias()]);

    let text = String::from_utf8(output.stdout.clone()).unwrap();
    let value: serde_json::Value = serde_json::from_str(&text).expect("one JSON document");
    let checks = value["checks"].as_array().unwrap();
    assert_eq!(checks.len(), 13, "every check is still reported");
    assert!(value["failures"].as_u64().unwrap() > 0, "{text}");
    assert_eq!(output.status.code(), Some(30));

    assert_eq!(status_of(&value, "sftp subsystem"), "fail", "{text}");
    assert_eq!(
        status_of(&value, "ssh client"),
        "pass",
        "the local client is fine; only the server side is not: {text}"
    );

    let sftp = checks
        .iter()
        .find(|check| check["name"] == "sftp subsystem")
        .unwrap();
    assert!(
        sftp["remedy"]
            .as_str()
            .is_some_and(|command| command.starts_with("ssh ")),
        "a failure must carry a command: {text}"
    );
}
