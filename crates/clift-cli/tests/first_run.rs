//! `clift setup` with no host, from the outside.
//!
//! The conversation itself is exercised in the binary's unit tests with a
//! scripted terminal. What only a real process can show is the door: without
//! an interactive terminal the command must refuse at once, name the
//! non-interactive commands, and leave stdout empty, rather than wait on a
//! question that nothing will answer.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::process::{Command, Output, Stdio};

fn clift(args: &[&str], stdin: Stdio) -> Output {
    let home = std::env::temp_dir().join(format!("clift-first-run-{}", std::process::id()));
    std::fs::create_dir_all(&home).unwrap();
    Command::new(env!("CARGO_BIN_EXE_clift"))
        .args(args)
        .env("XDG_CONFIG_HOME", &home)
        .env("XDG_CACHE_HOME", &home)
        .env("NO_COLOR", "1")
        .env_remove("CLIFT_RELAY_URL")
        .stdin(stdin)
        .output()
        .unwrap()
}

fn code(output: &Output) -> i32 {
    output.status.code().unwrap_or(-1)
}

#[test]
fn without_a_terminal_it_refuses_at_once_and_names_the_commands_that_do_not_ask() {
    // A pipe, not a terminal: the shape of every CI job and of `curl | sh`
    // without a `/dev/tty`. It must not read from it, and must not wait.
    let output = clift(&["setup"], Stdio::piped());
    assert_eq!(
        code(&output),
        20,
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stdout.is_empty(), "stdout must stay empty");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("not an interactive terminal"), "{stderr}");
    assert!(stderr.contains("clift config set relay.url"), "{stderr}");
    assert!(stderr.contains("clift setup <ssh-host> --yes"), "{stderr}");
}

#[test]
fn json_mode_is_refused_because_a_conversation_has_no_document() {
    let output = clift(&["--json", "setup"], Stdio::null());
    assert_eq!(code(&output), 20);
    assert!(output.stdout.is_empty(), "no partial JSON on stdout");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("--json is for machines"), "{stderr}");
}

#[test]
fn a_host_still_takes_the_fast_mode_path_and_still_refuses_without_yes() {
    // The old contract, unchanged by the optional host: a non-interactive
    // Fast Mode setup without --yes fails naming --yes, before dialling.
    let output = clift(&["setup", "no-such-host-for-this-test"], Stdio::null());
    assert_eq!(
        code(&output),
        20,
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stdout.is_empty());
}
