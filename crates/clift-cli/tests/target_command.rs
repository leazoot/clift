//! `clift target` end to end.
//!
//! No network is involved: these commands only ever edit the local
//! configuration, and the point of the tests is that a rejected argument does
//! not change it.

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
            "clift-target-{}-{label}-{unique}",
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

    fn config_file(&self) -> PathBuf {
        self.0.join("clift").join("config.toml")
    }

    fn config_text(&self) -> String {
        fs::read_to_string(self.config_file()).unwrap_or_default()
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

/// Acceptance 1: a row carries the name, the alias, whether it is the default
/// and when it last worked -- and nothing else.
#[test]
fn list_shows_the_alias_the_default_marker_and_the_last_success() {
    let scratch = Scratch::new("list");
    assert!(scratch.run(&["target", "add", "core"]).status.success());
    assert!(
        scratch
            .run(&["target", "add", "laptop", "--ssh-host", "my-laptop"])
            .status
            .success()
    );

    let listed = scratch.run(&["target", "list"]);
    assert!(listed.status.success());
    assert!(listed.stdout.is_empty(), "human output reached stdout");

    let text = stderr_of(&listed);
    assert!(text.contains("* core"), "the default is marked: {text}");
    assert!(text.contains("  laptop"), "{text}");
    assert!(text.contains("my-laptop"), "the alias is shown: {text}");
    assert!(
        text.contains("never connected"),
        "an unverified target says so rather than showing a blank: {text}"
    );
}

#[test]
fn the_json_listing_is_one_document_and_carries_no_key_material() {
    let scratch = Scratch::new("json");
    scratch.run(&["target", "add", "core"]);

    let listed = scratch.run(&["--json", "target", "list"]);
    let text = String::from_utf8(listed.stdout).unwrap();
    let value: serde_json::Value = serde_json::from_str(&text).expect("one JSON document");
    assert_eq!(value["schema_version"], 1);
    assert_eq!(value["targets"][0]["name"], "core");
    assert_eq!(value["targets"][0]["default"], true);
    assert_eq!(
        value["targets"][0]["last_success_at"],
        serde_json::Value::Null
    );
    for forbidden in ["identityfile", "id_ed25519", "token"] {
        assert!(!text.to_lowercase().contains(forbidden), "{text}");
    }
}

/// Acceptance 2: an illegal new name is refused, and the file is untouched.
#[test]
fn rename_refuses_a_name_with_a_control_character_and_changes_nothing() {
    let scratch = Scratch::new("rename");
    scratch.run(&["target", "add", "core"]);
    let before = scratch.config_text();

    let output = scratch.run(&["target", "rename", "core", "with\u{7}bell"]);
    assert_eq!(output.status.code(), Some(20), "{}", stderr_of(&output));
    assert!(output.stdout.is_empty());
    assert_eq!(
        scratch.config_text(),
        before,
        "a refused rename must not touch the configuration"
    );
}

#[test]
fn rename_moves_the_target_and_the_default_with_it() {
    let scratch = Scratch::new("rename-ok");
    scratch.run(&["target", "add", "core"]);
    assert!(
        scratch
            .run(&["target", "rename", "core", "work"])
            .status
            .success()
    );

    let text = scratch.config_text();
    assert!(text.contains("[targets.work]"), "{text}");
    assert!(!text.contains("[targets.core]"), "{text}");
    assert!(text.contains("default_target = \"work\""), "{text}");
}

/// Acceptance 3: the entry goes, the remote inbox stays, and the user is told
/// where it is.
#[test]
fn remove_keeps_the_remote_inbox_and_says_where_it_is() {
    let scratch = Scratch::new("remove");
    scratch.run(&["target", "add", "core"]);

    let output = scratch.run(&["target", "remove", "core"]);
    assert!(output.status.success(), "{}", stderr_of(&output));
    let text = stderr_of(&output);
    assert!(text.contains("still on core"), "{text}");
    assert!(text.contains(".cache/clift/inbox"), "{text}");
    assert!(
        text.contains("no default target now"),
        "removing the default must say so rather than pick another: {text}"
    );
    assert!(!scratch.config_text().contains("[targets.core]"));
}

#[test]
fn use_moves_the_default() {
    let scratch = Scratch::new("use");
    scratch.run(&["target", "add", "core"]);
    scratch.run(&["target", "add", "laptop"]);
    assert!(scratch.run(&["target", "use", "laptop"]).status.success());
    assert!(
        scratch
            .config_text()
            .contains("default_target = \"laptop\"")
    );
}

#[test]
fn an_unknown_target_is_refused_and_lists_what_exists() {
    let scratch = Scratch::new("unknown");
    scratch.run(&["target", "add", "core"]);
    let before = scratch.config_text();

    for args in [
        vec!["target", "use", "nope"],
        vec!["target", "rename", "nope", "other"],
        vec!["target", "remove", "nope"],
    ] {
        let output = scratch.run(&args);
        assert_eq!(output.status.code(), Some(20), "{args:?}");
        assert!(stderr_of(&output).contains("core"), "{args:?}");
        assert_eq!(scratch.config_text(), before, "{args:?}");
    }
}

/// Acceptance 4: the configuration is replaced whole, so a reader never sees a
/// partial document. The observable half of that is that the file is still
/// valid after every operation.
#[test]
fn every_operation_leaves_a_configuration_that_validates() {
    let scratch = Scratch::new("atomic");
    for args in [
        vec!["target", "add", "core"],
        vec!["target", "add", "laptop"],
        vec!["target", "use", "laptop"],
        vec!["target", "rename", "core", "work"],
        vec!["target", "remove", "work"],
    ] {
        assert!(scratch.run(&args).status.success(), "{args:?}");
        let validated = scratch.run(&["config", "validate"]);
        assert!(
            validated.status.success(),
            "configuration invalid after {args:?}: {}",
            stderr_of(&validated)
        );
    }
}
