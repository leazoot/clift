//! `clift status`, and the promise that neither it nor `doctor --json` can
//! print a key location or a token.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU32, Ordering};

static COUNTER: AtomicU32 = AtomicU32::new(0);

struct Scratch(PathBuf);

impl Scratch {
    fn new(label: &str) -> Self {
        let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "clift-status-{}-{label}-{unique}",
            std::process::id()
        ));
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }

    fn run(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_clift"))
            .args(args)
            .env("XDG_CONFIG_HOME", &self.0)
            .env("NO_COLOR", "1")
            .output()
            .expect("the clift binary must be runnable")
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn stderr_of(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

/// Acceptance 4: an empty installation gets a next step, not just a report of
/// emptiness.
#[test]
fn a_fresh_installation_is_told_what_to_do_next() {
    let scratch = Scratch::new("fresh");
    let output = scratch.run(&["status"]);

    assert!(output.status.success(), "{}", stderr_of(&output));
    assert!(output.stdout.is_empty(), "human output reached stdout");

    let text = stderr_of(&output);
    assert!(text.contains("No targets configured."), "{text}");
    assert!(
        text.contains("clift setup <ssh-host>"),
        "an empty status must offer the next command: {text}"
    );
    assert!(text.contains("clift "), "the version is shown: {text}");
}

#[test]
fn a_configured_installation_shows_targets_the_default_and_the_last_success() {
    let scratch = Scratch::new("configured");
    scratch.run(&["target", "add", "core"]);
    scratch.run(&["target", "add", "laptop", "--ssh-host", "my-laptop"]);

    let text = stderr_of(&scratch.run(&["status"]));
    assert!(text.contains("* core"), "the default is marked: {text}");
    assert!(text.contains("my-laptop"), "{text}");
    assert!(text.contains("never connected"), "{text}");
}

/// Acceptance 1 and 2.
#[test]
fn the_json_form_is_one_versioned_document() {
    let scratch = Scratch::new("json");
    scratch.run(&["target", "add", "core"]);

    let output = scratch.run(&["--json", "status"]);
    let text = String::from_utf8(output.stdout).unwrap();
    let value: serde_json::Value = serde_json::from_str(&text).expect("one JSON document");

    assert_eq!(
        text,
        serde_json::to_string(&value).unwrap(),
        "stdout carries bytes beyond the document itself"
    );
    assert_eq!(value["schema_version"], 1);
    assert_eq!(value["status"], "ok");
    assert_eq!(value["default_target"], "core");
    assert_eq!(value["targets"][0]["ssh_host"], "core");
    assert!(value["version"].as_str().is_some_and(|v| !v.is_empty()));
}

/// Acceptance 3: no key location, no token, in either machine output.
///
/// The target name is deliberately one that cannot resolve to a real machine.
/// `doctor` connects to whatever it is given, and a test has no business
/// reaching a host that belongs to whoever is running it.
#[test]
fn neither_status_nor_doctor_can_print_a_key_location_or_a_token() {
    let scratch = Scratch::new("secrets");
    scratch.run(&["target", "add", "clift-test-does-not-resolve"]);

    for args in [
        vec!["--json", "status"],
        vec!["--json", "doctor"],
        vec!["status"],
        vec!["target", "list"],
    ] {
        let output = scratch.run(&args);
        let combined = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            stderr_of(&output)
        )
        .to_lowercase();
        for forbidden in [
            "identityfile",
            "id_ed25519",
            "id_rsa",
            "id_ecdsa",
            "/.ssh/",
            "token",
            "bearer",
            "password",
        ] {
            assert!(
                !combined.contains(forbidden),
                "{args:?} printed {forbidden:?}:\n{combined}"
            );
        }
    }
}
