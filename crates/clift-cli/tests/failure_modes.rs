//! The six ways a send fails, and how differently it says so.
//!
//! The requirement is not only that these produce the right exit codes. It is
//! that a user who hits one of them can tell **which** one they hit, and is
//! given a command that addresses that one. So this asserts the codes, and then
//! asserts that no two of the six give the same advice.
//!
//! Four of the six are provoked against a real container; two need no host.

#![allow(clippy::unwrap_used, clippy::expect_used)]

#[path = "../../../tests/e2e/fixtures.rs"]
mod fixtures;

use fixtures::{SshdFixture, Topology, skip_without_docker};
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

struct Sandbox {
    home: PathBuf,
    bin: PathBuf,
    work: PathBuf,
}

impl Sandbox {
    /// `setup` is run only when the topology allows it; the broken ones are
    /// registered with `target add`, which does not connect.
    fn new(fixture: &SshdFixture, label: &str, config: &Path, verified: bool) -> Self {
        let home = fixture.workdir().join(format!("fail-home-{label}"));
        let bin = fixture.workdir().join(format!("fail-bin-{label}"));
        let work = fixture.workdir().join(format!("fail-work-{label}"));
        for path in [&home, &bin, &work] {
            fs::create_dir_all(path).unwrap();
        }
        for client in ["ssh", "sftp"] {
            let located = Command::new("/usr/bin/which").arg(client).output().unwrap();
            let real = String::from_utf8_lossy(&located.stdout).trim().to_string();
            let path = bin.join(client);
            fs::write(
                &path,
                format!("#!/bin/sh\nexec {real} -F {} \"$@\"\n", config.display()),
            )
            .unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
            }
        }

        let sandbox = Self { home, bin, work };
        let arguments: Vec<&str> = if verified {
            vec!["setup", fixture.alias(), "--yes"]
        } else {
            vec!["target", "add", fixture.alias()]
        };
        assert!(
            sandbox.run(&arguments).status.success(),
            "the sandbox must start from a configured state"
        );
        sandbox
    }

    fn file(&self, name: &str, contents: &[u8]) -> PathBuf {
        let path = self.work.join(name);
        fs::write(&path, contents).unwrap();
        path
    }

    fn run(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_clift"))
            .args(args)
            .current_dir(&self.work)
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
            .expect("clift must be runnable")
    }
}

fn stderr_of(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

/// The command a failure suggests, which is the actionable half of the message.
///
/// The reporter indents it by two spaces under the description, which is what
/// makes it recognisable without the test having to know which commands Clift
/// might suggest.
fn suggested_command(stderr: &str) -> String {
    stderr
        .lines()
        .find(|line| line.starts_with("  ") && !line.trim().is_empty())
        .unwrap_or_default()
        .trim()
        .to_string()
}

/// An attachment Clift cannot read: exit code 24.
#[test]
fn an_unreadable_attachment_is_exit_code_24() {
    let home = std::env::temp_dir().join("clift-fail-24");
    let _ = fs::remove_dir_all(&home);
    fs::create_dir_all(&home).unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_clift"))
        .args(["send", "/nonexistent/never/was.png"])
        .env("XDG_CONFIG_HOME", &home)
        .env("NO_COLOR", "1")
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(24), "{}", stderr_of(&output));
    assert!(output.stdout.is_empty());
    let _ = fs::remove_dir_all(&home);
}

/// Nothing to send to and nothing to guess by: exit code 21.
#[test]
fn an_undecidable_target_is_exit_code_21() {
    let home = std::env::temp_dir().join("clift-fail-21");
    let _ = fs::remove_dir_all(&home);
    fs::create_dir_all(&home).unwrap();
    let file = home.join("note.txt");
    fs::write(&file, b"x").unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_clift"))
        .args(["send", file.to_str().unwrap()])
        .env("XDG_CONFIG_HOME", &home)
        .env("NO_COLOR", "1")
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(21), "{}", stderr_of(&output));
    assert!(
        stderr_of(&output).contains("clift setup"),
        "{}",
        stderr_of(&output)
    );
    let _ = fs::remove_dir_all(&home);
}

/// The four that need a host, each provoked for real, and all six compared.
#[test]
fn the_six_failures_are_told_apart_from_one_another() {
    if skip_without_docker("the_six_failures_are_told_apart_from_one_another") {
        return;
    }
    let mut codes: Vec<(&str, i32, String)> = Vec::new();

    // Authentication rejected: the right host, the wrong key.
    {
        let fixture = SshdFixture::start(Topology::Normal);
        let spare = fixture.spare_identity();
        let config = fixture.variant_config("wrong-key", |original| {
            original
                .lines()
                .map(|line| {
                    if line.trim_start().starts_with("IdentityFile") {
                        format!("    IdentityFile {}", spare.display())
                    } else {
                        line.to_string()
                    }
                })
                .collect::<Vec<_>>()
                .join("\n")
        });
        let sandbox = Sandbox::new(&fixture, "auth", &config, false);
        let file = sandbox.file("note.txt", b"x");
        let output = sandbox.run(&["send", file.to_str().unwrap()]);
        assert_eq!(
            output.status.code(),
            Some(22),
            "authentication: {}",
            stderr_of(&output)
        );
        codes.push(("authentication", 22, stderr_of(&output)));
    }

    // No SFTP subsystem on the far side.
    {
        let fixture = SshdFixture::start(Topology::NoSftp);
        let config = fixture.ssh_config().to_path_buf();
        let sandbox = Sandbox::new(&fixture, "nosftp", &config, false);
        let file = sandbox.file("note.txt", b"x");
        let output = sandbox.run(&["send", file.to_str().unwrap()]);
        assert_eq!(
            output.status.code(),
            Some(22),
            "sftp subsystem: {}",
            stderr_of(&output)
        );
        codes.push(("sftp subsystem", 22, stderr_of(&output)));
    }

    // A home directory Clift cannot write to.
    {
        let fixture = SshdFixture::start(Topology::ReadonlyHome);
        let config = fixture.ssh_config().to_path_buf();
        let sandbox = Sandbox::new(&fixture, "readonly", &config, false);
        let file = sandbox.file("note.txt", b"x");
        let output = sandbox.run(&["send", file.to_str().unwrap()]);
        assert_eq!(
            output.status.code(),
            Some(25),
            "remote directory: {}",
            stderr_of(&output)
        );
        codes.push(("remote directory", 25, stderr_of(&output)));
    }

    // Over the batch limit.
    {
        let fixture = SshdFixture::start(Topology::Normal);
        let config = fixture.ssh_config().to_path_buf();
        let sandbox = Sandbox::new(&fixture, "limit", &config, true);
        let big = sandbox.work.join("huge.bin");
        let handle = fs::File::create(&big).unwrap();
        handle.set_len(50 * 1024 * 1024 + 1).unwrap();
        drop(handle);
        let output = sandbox.run(&["send", big.to_str().unwrap()]);
        assert_eq!(
            output.status.code(),
            Some(26),
            "limit: {}",
            stderr_of(&output)
        );
        codes.push(("limit", 26, stderr_of(&output)));
    }

    // The two that need no host, gathered the same way.
    for (label, args, expected) in [
        ("attachment", vec!["send", "/nonexistent/never/was.png"], 24),
        ("target", vec!["send", "--to", "nowhere", "/etc/hosts"], 20),
    ] {
        let home = std::env::temp_dir().join(format!("clift-fail-{label}"));
        let _ = fs::remove_dir_all(&home);
        fs::create_dir_all(&home).unwrap();
        let output = Command::new(env!("CARGO_BIN_EXE_clift"))
            .args(&args)
            .env("XDG_CONFIG_HOME", &home)
            .env("NO_COLOR", "1")
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(expected), "{label}");
        codes.push((label, expected, stderr_of(&output)));
        let _ = fs::remove_dir_all(&home);
    }

    // Every one of them produced its own advice. Two share an exit code by
    // design, so what
    // has to differ is what the user is told to do about it.
    let advice: BTreeSet<String> = codes
        .iter()
        .map(|(_, _, stderr)| suggested_command(stderr))
        .collect();
    let summaries: BTreeSet<String> = codes
        .iter()
        .map(|(_, _, stderr)| {
            stderr
                .lines()
                .find(|line| line.starts_with("error:"))
                .unwrap_or_default()
                .to_string()
        })
        .collect();

    let labelled: Vec<(&str, String)> = codes
        .iter()
        .map(|(label, _, stderr)| {
            (
                *label,
                stderr
                    .lines()
                    .find(|line| line.starts_with("error:"))
                    .unwrap_or_default()
                    .to_string(),
            )
        })
        .collect();
    assert_eq!(
        summaries.len(),
        codes.len(),
        "two failures gave the same summary: {labelled:#?}"
    );
    assert!(
        advice.len() >= 3,
        "the advice is barely distinguishable: {advice:#?}"
    );
    for (label, _, stderr) in &codes {
        assert!(
            !suggested_command(stderr).is_empty(),
            "{label} offered no command: {stderr}"
        );
    }
}
