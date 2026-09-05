//! `clift config` end to end, including the output channel contract.

// A panic here is a test failure, not a user-facing crash; see clippy.toml.
#![allow(clippy::unwrap_used)]

use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU32, Ordering};

static COUNTER: AtomicU32 = AtomicU32::new(0);

/// An isolated XDG_CONFIG_HOME so tests never touch the developer's own config.
struct Scratch(PathBuf);

impl Scratch {
    fn new(label: &str) -> Self {
        let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "clift-cfgcmd-{}-{label}-{unique}",
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
            .unwrap()
    }

    fn config_file(&self) -> PathBuf {
        self.0.join("clift").join("config.toml")
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn code(output: &Output) -> i32 {
    output.status.code().unwrap()
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).to_string()
}

#[test]
fn path_reports_the_resolved_location_and_honours_xdg_config_home() {
    let scratch = Scratch::new("path");
    let output = scratch.run(&["config", "path"]);

    assert_eq!(code(&output), 0);
    let printed = stdout(&output);
    let printed = printed.trim_end();
    assert_eq!(PathBuf::from(printed), scratch.config_file());
    assert!(
        PathBuf::from(printed).is_absolute(),
        "the reported path must be absolute: {printed}"
    );
}

#[test]
fn path_in_json_mode_is_one_document_with_no_extra_bytes() {
    let scratch = Scratch::new("pathjson");
    let output = scratch.run(&["config", "path", "--json"]);

    assert_eq!(code(&output), 0);
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        output.stdout,
        serde_json::to_string(&value).unwrap().into_bytes()
    );
    assert_eq!(value["schema_version"], 1);
    assert_eq!(value["exists"], false);
}

#[test]
fn validate_accepts_a_missing_config_and_reports_on_stderr_only() {
    let scratch = Scratch::new("validate-missing");
    let output = scratch.run(&["config", "validate"]);

    assert_eq!(code(&output), 0);
    assert!(
        output.stdout.is_empty(),
        "a human readable report must not reach stdout: {:?}",
        stdout(&output)
    );
    assert!(String::from_utf8_lossy(&output.stderr).contains("valid"));
}

#[test]
fn validate_rejects_a_broken_config_with_code_20_and_names_the_field() {
    let scratch = Scratch::new("validate-broken");
    fs::create_dir_all(scratch.config_file().parent().unwrap()).unwrap();
    fs::write(
        scratch.config_file(),
        "version = 1\n[defaults]\nmax_files = \"twenty\"\n",
    )
    .unwrap();

    let output = scratch.run(&["config", "validate"]);
    assert_eq!(code(&output), 20);
    assert!(output.stdout.is_empty());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("max_files"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn validate_warns_about_unknown_keys_without_failing() {
    let scratch = Scratch::new("validate-unknown");
    fs::create_dir_all(scratch.config_file().parent().unwrap()).unwrap();
    fs::write(scratch.config_file(), "version = 1\nmax_filez = 21\n").unwrap();

    let output = scratch.run(&["config", "validate"]);
    assert_eq!(code(&output), 0);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("warning"), "{stderr}");
    assert!(stderr.contains("max_filez"), "{stderr}");
}

#[test]
fn set_writes_valid_toml_with_private_permissions() {
    let scratch = Scratch::new("set");
    assert_eq!(
        code(&scratch.run(&["config", "set", "targets.core.ssh_host", "core"])),
        0
    );
    assert_eq!(
        code(&scratch.run(&["config", "set", "default_target", "core"])),
        0
    );
    assert_eq!(
        code(&scratch.run(&["config", "set", "defaults.max_files", "15"])),
        0
    );

    let source = fs::read_to_string(scratch.config_file()).unwrap();
    let document: toml::Table = source.parse().expect("written file must be valid TOML");
    assert_eq!(document["version"].as_integer(), Some(1));
    assert_eq!(
        document["defaults"]["max_files"].as_integer(),
        Some(15),
        "a numeric key must be stored as an integer, not a string"
    );

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(scratch.config_file())
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600, "config file mode is {mode:o}");
    }

    assert_eq!(code(&scratch.run(&["config", "validate"])), 0);
}

#[test]
fn set_keeps_keys_this_build_does_not_recognise() {
    let scratch = Scratch::new("set-preserve");
    fs::create_dir_all(scratch.config_file().parent().unwrap()).unwrap();
    fs::write(
        scratch.config_file(),
        "version = 1\nexperimental_key = true\n\n[targets.core]\nssh_host = \"core\"\n",
    )
    .unwrap();

    assert_eq!(
        code(&scratch.run(&["config", "set", "default_target", "core"])),
        0
    );

    let source = fs::read_to_string(scratch.config_file()).unwrap();
    let document: toml::Table = source.parse().unwrap();
    assert_eq!(
        document["experimental_key"].as_bool(),
        Some(true),
        "an edit must not delete a key it merely warned about"
    );
    assert_eq!(
        document["targets"]["core"]["ssh_host"].as_str(),
        Some("core")
    );
}

#[test]
fn set_rejects_a_value_that_would_make_the_config_invalid() {
    let scratch = Scratch::new("set-invalid");
    assert_eq!(
        code(&scratch.run(&["config", "set", "targets.core.ssh_host", "core"])),
        0
    );
    let before = fs::read_to_string(scratch.config_file()).unwrap();

    let output = scratch.run(&["config", "set", "defaults.max_files", "0"]);
    assert_eq!(code(&output), 20);
    assert!(output.stdout.is_empty());
    assert_eq!(
        fs::read_to_string(scratch.config_file()).unwrap(),
        before,
        "a rejected edit must not be written"
    );
}

#[test]
fn get_reads_back_what_set_wrote() {
    let scratch = Scratch::new("get");
    scratch.run(&["config", "set", "targets.core.ssh_host", "core"]);
    scratch.run(&["config", "set", "defaults.max_files", "7"]);

    let output = scratch.run(&["config", "get", "targets.core.ssh_host"]);
    assert_eq!(code(&output), 0);
    assert_eq!(stdout(&output).trim_end(), "core");

    let output = scratch.run(&["config", "get", "defaults.max_files"]);
    assert_eq!(stdout(&output).trim_end(), "7");
}

#[test]
fn get_and_set_reject_unknown_keys_with_code_20() {
    let scratch = Scratch::new("unknown");
    for args in [
        vec!["config", "get", "defaults.max_filez"],
        vec!["config", "set", "defaults.max_filez", "1"],
    ] {
        let output = scratch.run(&args);
        assert_eq!(code(&output), 20, "{args:?}");
        assert!(output.stdout.is_empty(), "{args:?}");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains("defaults.max_files"),
            "the error should list the valid keys: {stderr}"
        );
    }
}

#[test]
fn get_an_unset_key_says_how_to_set_it() {
    let scratch = Scratch::new("unset");
    let output = scratch.run(&["config", "get", "default_target"]);
    assert_eq!(code(&output), 20);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("clift config set default_target"),
        "{stderr}"
    );
}

/// The summary is what a user reads, the cause chain is what they get
/// when they ask for it. Without this, an adapter could attach a perfectly good
/// cause and no one would ever see it.
#[test]
fn the_cause_chain_appears_only_under_debug() {
    let scratch = Scratch::new("causechain");
    fs::create_dir_all(scratch.config_file().parent().unwrap()).unwrap();
    fs::write(scratch.config_file(), "version = 1\ndefaults = [\n").unwrap();

    let plain = scratch.run(&["config", "validate"]);
    let debug = scratch.run(&["config", "validate", "--debug"]);

    assert_eq!(code(&plain), 20);
    assert_eq!(code(&debug), 20);
    assert!(
        stdout(&plain).is_empty(),
        "a failure must leave stdout empty"
    );
    assert!(
        stdout(&debug).is_empty(),
        "a failure must leave stdout empty"
    );

    let plain_err = String::from_utf8_lossy(&plain.stderr).to_string();
    let debug_err = String::from_utf8_lossy(&debug.stderr).to_string();
    assert!(
        !plain_err.contains("caused by:"),
        "normal mode must show the summary alone: {plain_err}"
    );
    assert!(
        debug_err.contains("caused by:"),
        "--debug must show the cause chain: {debug_err}"
    );
}
