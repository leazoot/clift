//! The output channel contract (architecture 2.2, frontend rules 1).
//!
//! These assertions are what stand between Clift and the failure mode that
//! matters most: a stray byte on stdout gets typed into the agent's prompt by
//! the terminal integration.

// A panic here is a test failure, not a user-facing crash; see clippy.toml.
#![allow(clippy::unwrap_used)]

use std::process::{Command, Output};
use std::sync::atomic::{AtomicU32, Ordering};

static COUNTER: AtomicU32 = AtomicU32::new(0);

/// An empty configuration directory of its own.
///
/// Without it a command like `doctor` would read the developer's real
/// configuration and connect to whatever host is in it, which is a side effect
/// no test is entitled to.
fn isolated() -> std::path::PathBuf {
    let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!("clift-contract-{}-{unique}", std::process::id()));
    std::fs::create_dir_all(&path).unwrap();
    path
}

fn clift(args: &[&str]) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_clift"));
    command.args(args);
    // Colour must be off for a non-terminal anyway; setting this proves the
    // NO_COLOR convention is honoured rather than relying on that.
    command.env("NO_COLOR", "1");
    command.env("XDG_CONFIG_HOME", isolated());
    command.output().unwrap()
}

fn clift_colored(args: &[&str]) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_clift"));
    command.args(args);
    command.env_remove("NO_COLOR");
    command.env("XDG_CONFIG_HOME", isolated());
    command.output().unwrap()
}

fn code(output: &Output) -> i32 {
    output.status.code().unwrap()
}

#[test]
fn json_stdout_is_exactly_one_document_with_no_extra_bytes() {
    let output = clift(&["--version", "--json"]);
    assert_eq!(code(&output), 0);

    let value: serde_json::Value = serde_json::from_slice(&output.stdout)
        .unwrap_or_else(|e| panic!("stdout is not valid JSON: {e}: {:?}", output.stdout));

    assert_eq!(
        output.stdout,
        serde_json::to_string(&value).unwrap().into_bytes(),
        "stdout carries bytes beyond the JSON document itself"
    );
    assert_eq!(value["schema_version"], 1);
    assert!(value["commit"].as_str().is_some_and(|c| !c.is_empty()));
}

#[test]
fn a_failing_command_writes_nothing_at_all_to_stdout() {
    for args in [
        vec!["doctor", "nope"],
        vec!["--json", "doctor", "nope"],
        vec!["send", "/nonexistent/never/was.png"],
        // `doctor` itself succeeds now; a target that does not exist is the
        // failure path worth checking here.
        vec!["doctor", "nope"],
        vec!["send", "a.png"],
        vec!["nope"],
        vec!["--not-a-flag"],
    ] {
        let output = clift(&args);
        assert_ne!(code(&output), 0, "{args:?} unexpectedly succeeded");
        assert!(
            output.stdout.is_empty(),
            "{args:?} wrote {:?} to stdout on a failure path",
            String::from_utf8_lossy(&output.stdout)
        );
    }
}

/// A command that runs and fails for a real reason.
///
/// This used to be a command the build did not implement yet. Every command in
/// The specification's tree is implemented now, so the failure being checked is an
/// ordinary one: a target that does not exist.
const FAILS: [&str; 3] = ["doctor", "nope", "--verbose"];

/// The reported commit must be the one the binary was built from.
///
/// It went stale once: the build script watched `.git/HEAD`, which on a branch
/// holds `ref: refs/heads/…` and does not change when a commit is made. A
/// version string that names the wrong commit is worse than one that names
/// none, because it is wrong in a way nobody would think to check.
#[test]
fn the_reported_commit_is_the_one_this_binary_was_built_from() {
    let output = Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output();
    let Ok(output) = output else {
        return; // No git here; nothing to compare against.
    };
    if !output.status.success() {
        return;
    }
    let head = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if head.is_empty() {
        return;
    }

    let reported = clift(&["--version"]);
    let text = String::from_utf8_lossy(&reported.stdout);
    assert!(
        text.contains(&head),
        "version reports a different commit than HEAD ({head}): {text}"
    );
}

#[test]
fn errors_and_progress_go_to_stderr() {
    let output = clift(&FAILS);
    assert!(output.stdout.is_empty());

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("running doctor"), "{stderr}");
    assert!(stderr.contains("no target called"), "{stderr}");
}

#[test]
fn exit_codes_come_from_the_core_mapping() {
    assert_eq!(code(&clift(&["--version"])), 0);
    assert_eq!(code(&clift(&["--help"])), 0);
    // A usage error is reported as a configuration error rather than as an
    // eleventh exit code that the specification does not define.
    assert_eq!(code(&clift(&["nope"])), 20);
    assert_eq!(code(&clift(&["--not-a-flag"])), 20);
    assert_eq!(code(&clift(&["send", "--to"])), 20);
    // A configuration problem, from the one mapping in clift-core.
    assert_eq!(code(&clift(&["doctor", "nope"])), 20);
    // An attachment that cannot be read is a clipboard-stage failure.
    assert_eq!(code(&clift(&["send", "/nonexistent/never/was.png"])), 24);
}

#[test]
fn unknown_subcommands_fail_instead_of_being_guessed() {
    // `stat` is one character from `status`; it must not be accepted as it.
    let output = clift(&["stat"]);
    assert_eq!(code(&output), 20);
    assert!(output.stdout.is_empty());

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("running status"),
        "a near miss must not be executed: {stderr}"
    );
}

#[test]
fn no_ansi_escapes_when_not_a_terminal() {
    for output in [clift(&["status"]), clift_colored(&["status"])] {
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            !stderr.contains('\u{1b}'),
            "ANSI escape written to a non-terminal: {stderr:?}"
        );
    }
}

#[test]
fn the_debug_flag_adds_the_cause_chain_and_nothing_to_stdout() {
    // A failure with a cause underneath it: the file cannot be read, and the
    // operating system said why.
    let plain = clift(&["send", "/nonexistent/never/was.png"]);
    let debug = clift(&["send", "/nonexistent/never/was.png", "--debug"]);
    assert!(debug.stdout.is_empty());
    assert!(plain.stdout.is_empty());

    let plain = String::from_utf8_lossy(&plain.stderr);
    let debug = String::from_utf8_lossy(&debug.stderr);
    assert!(!plain.contains("caused by:"), "{plain}");
    assert!(debug.contains("caused by:"), "{debug}");
}

#[test]
fn help_lists_only_commands_this_build_actually_has() {
    let output = clift(&["--help"]);
    assert_eq!(code(&output), 0);
    let stdout = String::from_utf8_lossy(&output.stdout);

    for expected in [
        "setup",
        "send",
        "paste",
        "copy",
        "target",
        "doctor",
        "status",
        "clean",
        "config",
        "uninstall",
    ] {
        assert!(stdout.contains(expected), "help omits {expected}: {stdout}");
    }
    // Not built: advertising any of these would claim support that does not
    // exist. `integrate` and `ssh` were built once and have been withdrawn; a
    // help entry would send a user looking for a subcommand that is gone.
    for absent in ["clift update", "integrate", "ssh"] {
        assert!(
            !stdout.contains(absent),
            "help advertises {absent}, which this build does not have: {stdout}"
        );
    }
}
