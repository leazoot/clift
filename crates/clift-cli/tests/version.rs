//! The specification requires `clift --version` to report version, commit and target
//! triple. A missing or empty field makes a bug report unattributable, so each
//! one is asserted separately rather than against a single golden string.

// Same rationale as `allow-unwrap-in-tests` in clippy.toml: a panic in a test
// binary is a test failure, not a user-facing crash. The clippy.toml switch only
// covers `#[test]` functions, so helpers in this file need the allow explicitly.
#![allow(clippy::unwrap_used)]

use std::process::Command;

fn version_output() -> String {
    let out = Command::new(env!("CARGO_BIN_EXE_clift"))
        .arg("--version")
        .output()
        .unwrap();
    assert!(out.status.success(), "--version exited with {}", out.status);
    String::from_utf8(out.stdout).unwrap()
}

#[test]
fn version_reports_all_three_fields() {
    let text = version_output();
    let (_, rest) = text.trim().split_once(' ').unwrap();
    let (version, tail) = rest.split_once(' ').unwrap();
    let inner = tail.trim_start_matches('(').trim_end_matches(')');
    let (commit, target) = inner.split_once(' ').unwrap();

    assert_eq!(version, env!("CARGO_PKG_VERSION"));
    assert!(!commit.is_empty(), "commit field is empty: {text:?}");
    assert!(!target.is_empty(), "target field is empty: {text:?}");
    assert!(
        target.contains('-'),
        "target does not look like a triple: {target:?}"
    );
}

#[test]
fn short_and_long_version_flags_agree() {
    let long = version_output();
    let short = Command::new(env!("CARGO_BIN_EXE_clift"))
        .arg("-V")
        .output()
        .unwrap();
    assert_eq!(long.as_bytes(), short.stdout.as_slice());
}
