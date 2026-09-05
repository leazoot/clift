//! `clift clean` end to end.

#![allow(clippy::unwrap_used, clippy::expect_used)]

#[path = "../../../tests/e2e/fixtures.rs"]
mod fixtures;

use fixtures::{SshdFixture, Topology, skip_without_docker};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

struct Sandbox {
    home: PathBuf,
    bin: PathBuf,
}

impl Sandbox {
    fn new(fixture: &SshdFixture, label: &str) -> Self {
        let home = fixture.workdir().join(format!("clean-home-{label}"));
        let bin = fixture.workdir().join(format!("clean-bin-{label}"));
        fs::create_dir_all(&home).unwrap();
        fs::create_dir_all(&bin).unwrap();
        shim(&bin, fixture);

        let sandbox = Self { home, bin };
        assert!(
            sandbox
                .run(&["setup", fixture.alias(), "--yes"])
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
            .expect("clift must be runnable")
    }
}

fn shim(bin: &Path, fixture: &SshdFixture) {
    for client in ["ssh", "sftp"] {
        let located = Command::new("/usr/bin/which").arg(client).output().unwrap();
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

fn stderr_of(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn shell(fixture: &SshdFixture, command: &str) -> String {
    String::from_utf8_lossy(&fixture.ssh(command).stdout)
        .trim()
        .to_string()
}

/// One batch from long ago and one from today.
fn seed(fixture: &SshdFixture) -> (String, String) {
    let inbox = format!("{}/.cache/clift/inbox", fixture.remote_home());
    let old = format!("{inbox}/2026-08-01/aaaa");
    let new = format!("{inbox}/2026-08-30/bbbb");
    assert!(
        fixture
            .ssh(&format!(
                "mkdir -p '{old}' '{new}' && printf oldfile > '{old}/a.png' && \
                 printf newfile > '{new}/b.png' && \
                 touch -d '2020-01-01 00:00' '{old}/a.png' '{old}'"
            ))
            .status
            .success()
    );
    (old, new)
}

#[test]
fn the_default_run_removes_only_what_is_past_the_retention() {
    if skip_without_docker("the_default_run_removes_only_what_is_past_the_retention") {
        return;
    }
    let fixture = SshdFixture::start(Topology::Normal);
    let sandbox = Sandbox::new(&fixture, "default");
    let (old, new) = seed(&fixture);

    let output = sandbox.run(&["clean"]);
    assert!(output.status.success(), "{}", stderr_of(&output));
    assert!(output.stdout.is_empty(), "human output reached stdout");

    let text = stderr_of(&output);
    assert!(text.contains("Removed 1 batch"), "{text}");
    assert!(text.contains("file"), "{text}");
    assert_eq!(
        shell(&fixture, &format!("test -d '{old}' && echo yes || echo no")),
        "no"
    );
    assert_eq!(shell(&fixture, &format!("cat '{new}/b.png'")), "newfile");
}

#[test]
fn older_than_moves_the_line() {
    if skip_without_docker("older_than_moves_the_line") {
        return;
    }
    let fixture = SshdFixture::start(Topology::Normal);
    let sandbox = Sandbox::new(&fixture, "older");
    let (old, new) = seed(&fixture);

    // Ten years is longer than anything here, so nothing goes.
    let output = sandbox.run(&["clean", "--older-than", "3650d"]);
    assert!(output.status.success(), "{}", stderr_of(&output));
    assert!(
        stderr_of(&output).contains("Nothing to remove"),
        "{}",
        stderr_of(&output)
    );
    assert_eq!(
        shell(&fixture, &format!("test -d '{old}' && echo yes || echo no")),
        "yes"
    );
    assert_eq!(
        shell(&fixture, &format!("test -d '{new}' && echo yes || echo no")),
        "yes"
    );
}

/// `--all` on a machine with nobody to ask fails rather than waiting, and
/// removes nothing.
#[test]
fn all_without_yes_refuses_in_a_non_interactive_run() {
    if skip_without_docker("all_without_yes_refuses_in_a_non_interactive_run") {
        return;
    }
    let fixture = SshdFixture::start(Topology::Normal);
    let sandbox = Sandbox::new(&fixture, "all-no-yes");
    let (old, new) = seed(&fixture);

    let output = sandbox.run(&["clean", "--all"]);
    assert_eq!(output.status.code(), Some(20), "{}", stderr_of(&output));
    assert!(
        stderr_of(&output).contains("--yes"),
        "{}",
        stderr_of(&output)
    );
    assert_eq!(
        shell(&fixture, &format!("test -d '{old}' && echo yes || echo no")),
        "yes"
    );
    assert_eq!(
        shell(&fixture, &format!("test -d '{new}' && echo yes || echo no")),
        "yes"
    );
}

#[test]
fn all_with_yes_removes_everything_and_reports_what_it_freed() {
    if skip_without_docker("all_with_yes_removes_everything_and_reports_what_it_freed") {
        return;
    }
    let fixture = SshdFixture::start(Topology::Normal);
    let sandbox = Sandbox::new(&fixture, "all-yes");
    seed(&fixture);

    let output = sandbox.run(&["--json", "clean", "--all", "--yes"]);
    assert!(output.status.success(), "{}", stderr_of(&output));
    let value: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("one JSON document");
    assert_eq!(value["batches"], 2);
    assert_eq!(value["files"], 2);
    assert_eq!(value["bytes"], 14, "oldfile + newfile");
    assert_eq!(value["dry_run"], false);

    let inbox = format!("{}/.cache/clift/inbox", fixture.remote_home());
    assert_eq!(
        shell(&fixture, &format!("find '{inbox}' -type f | wc -l")),
        "0"
    );
}

/// A dry run says what it would do and does none of it.
#[test]
fn a_dry_run_changes_nothing() {
    if skip_without_docker("a_dry_run_changes_nothing") {
        return;
    }
    let fixture = SshdFixture::start(Topology::Normal);
    let sandbox = Sandbox::new(&fixture, "dry");
    let (old, new) = seed(&fixture);

    let output = sandbox.run(&["clean", "--all", "--dry-run"]);
    assert!(output.status.success(), "{}", stderr_of(&output));
    assert!(
        stderr_of(&output).contains("Would remove 2 batch"),
        "{}",
        stderr_of(&output)
    );
    assert_eq!(shell(&fixture, &format!("cat '{old}/a.png'")), "oldfile");
    assert_eq!(shell(&fixture, &format!("cat '{new}/b.png'")), "newfile");
}

#[test]
fn an_unconfigured_target_is_refused() {
    if skip_without_docker("an_unconfigured_target_is_refused") {
        return;
    }
    let fixture = SshdFixture::start(Topology::Normal);
    let sandbox = Sandbox::new(&fixture, "unknown");

    let output = sandbox.run(&["clean", "nowhere"]);
    assert_eq!(output.status.code(), Some(20), "{}", stderr_of(&output));
    assert!(output.stdout.is_empty());
}
