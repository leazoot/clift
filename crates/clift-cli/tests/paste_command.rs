//! `clift paste`, which is what a terminal calls instead of pasting.
//!
//! The exit code is the whole interface, so that is what these check -- and
//! that stdout carries nothing a terminal would be wrong to type.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;
use std::io::Write as _;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Mutex, MutexGuard};
use std::time::Instant;

static COUNTER: AtomicU32 = AtomicU32::new(0);
static PASTEBOARD: Mutex<()> = Mutex::new(());

fn exclusive() -> MutexGuard<'static, ()> {
    PASTEBOARD
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn announce(line: &str) {
    let mut stderr = std::io::stderr();
    let _ = writeln!(stderr, "{line}");
    let _ = stderr.flush();
}

fn skip(test: &str) -> bool {
    if std::env::var_os("CLIFT_REAL_CLIPBOARD").is_some() {
        return false;
    }
    announce(&format!(
        "SKIPPED {test}: CLIFT_REAL_CLIPBOARD is not set (it overwrites the \
         real clipboard); this test proved nothing"
    ));
    true
}

fn put_text(text: &str) {
    let mut child = Command::new("pbcopy")
        .stdin(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(text.as_bytes())
        .unwrap();
    assert!(child.wait().unwrap().success());
}

fn isolated() -> PathBuf {
    let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!("clift-paste-{unique}"));
    let _ = fs::remove_dir_all(&path);
    fs::create_dir_all(&path).unwrap();
    path
}

fn clift(home: &PathBuf, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_clift"))
        .args(args)
        .env("XDG_CONFIG_HOME", home)
        .env("NO_COLOR", "1")
        .output()
        .expect("clift must be runnable")
}

/// Plain text: exit code 10, and a document that says so without prose.
///
/// The document matters as much as the code: a caller that only sees whether
/// the process succeeded tells "nothing to send" from "something broke" by
/// reading this.
#[test]
fn plain_text_is_exit_code_ten_and_a_machine_readable_reason() {
    if skip("plain_text_is_exit_code_ten_and_a_machine_readable_reason") {
        return;
    }
    let _turn = exclusive();
    let home = isolated();
    put_text("an ordinary line of text");

    let output = clift(&home, &["--json", "paste"]);
    assert_eq!(output.status.code(), Some(10));

    let text = String::from_utf8(output.stdout).unwrap();
    let value: serde_json::Value = serde_json::from_str(&text).expect("one JSON document");
    assert_eq!(value["schema_version"], 1);
    assert_eq!(value["status"], "no_attachment");
    assert_eq!(
        text,
        serde_json::to_string(&value).unwrap(),
        "stdout carries bytes beyond the document"
    );

    let _ = fs::remove_dir_all(&home);
}

/// Without `--json` the same case prints nothing at all on stdout.
#[test]
fn plain_text_prints_nothing_a_terminal_could_type() {
    if skip("plain_text_prints_nothing_a_terminal_could_type") {
        return;
    }
    let _turn = exclusive();
    let home = isolated();
    put_text("an ordinary line of text");

    let output = clift(&home, &["paste"]);
    assert_eq!(output.status.code(), Some(10));
    assert!(
        output.stdout.is_empty(),
        "{:?}",
        String::from_utf8_lossy(&output.stdout)
    );
    let _ = fs::remove_dir_all(&home);
}

/// Deciding that the clipboard holds only text must be quick, because it
/// happens on every ordinary paste. The budget is 80 ms for the clipboard read;
/// this measures the whole process, which is a stricter thing to ask.
#[test]
fn deciding_there_is_nothing_to_send_is_quick() {
    if skip("deciding_there_is_nothing_to_send_is_quick") {
        return;
    }
    let _turn = exclusive();
    let home = isolated();
    put_text("an ordinary line of text");

    // One warm-up, then the measurement: the first run pays for the loader.
    let _ = clift(&home, &["paste"]);
    let mut worst = std::time::Duration::ZERO;
    for _ in 0..10 {
        let started = Instant::now();
        let output = clift(&home, &["paste"]);
        worst = worst.max(started.elapsed());
        assert_eq!(output.status.code(), Some(10));
    }
    announce(&format!(
        "paste on plain text, worst of ten whole-process runs: {worst:?}"
    ));
    assert!(
        worst < std::time::Duration::from_millis(500),
        "an ordinary paste took {worst:?}, which the user would feel"
    );
    let _ = fs::remove_dir_all(&home);
}

/// A failure leaves stdout completely empty, which is what makes a caller's
/// failure branch safe: there is nothing for it to type.
#[test]
fn a_failure_leaves_nothing_on_stdout() {
    let home = isolated();
    let output = clift(&home, &["paste", "--mode", "kitty"]);
    assert_ne!(output.status.code(), Some(0));
    assert!(output.stdout.is_empty());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("kitty"),
        "the message names what was asked for"
    );
    let _ = fs::remove_dir_all(&home);
}
