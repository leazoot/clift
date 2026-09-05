//! A clipboard image must not outlive the run that made it.
//!
//! Four ways out of a program, one cleanup. `Drop` covers the ordinary return
//! and the early `?`; a panic unwinds and so is covered too; a signal is not,
//! and that is what the last test is for. It sends a real SIGINT to a real
//! process, because "does the file survive a Ctrl+C" cannot be answered any
//! other way.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use clift_core::runtime::ScratchFile;
use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

#[test]
fn a_scratch_file_is_private_and_outside_the_public_temp_directory() {
    let scratch = ScratchFile::create("lifetime", "bin", b"payload").unwrap();
    let path = scratch.path().to_path_buf();

    assert!(path.exists());
    for public in ["/tmp", "/var/tmp", "/dev/shm"] {
        assert!(
            !path.starts_with(public),
            "a private file must not live in {public}: {}",
            path.display()
        );
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }
}

#[test]
fn a_normal_return_removes_the_file() {
    let path = {
        let scratch = ScratchFile::create("lifetime-ok", "bin", b"x").unwrap();
        scratch.path().to_path_buf()
    };
    assert!(!path.exists(), "{}", path.display());
}

/// An unwinding panic still runs `Drop`, so the file goes with it.
#[test]
fn a_panic_removes_the_file() {
    let captured: PathBuf = std::panic::catch_unwind(|| {
        let scratch = ScratchFile::create("lifetime-panic", "bin", b"x").unwrap();
        let path = scratch.path().to_path_buf();
        // The path is carried out through the panic payload, so the test can
        // look for the file after the stack has unwound past the guard.
        std::panic::panic_any(path);
    })
    .expect_err("the closure panics on purpose")
    .downcast::<PathBuf>()
    .map(|path| *path)
    .expect("the payload is the path");

    assert!(
        !captured.exists(),
        "a panic left the file behind: {}",
        captured.display()
    );
}

/// Acceptance 4: a real signal, a real process, a real look at the filesystem.
/// Where cargo puts the example this test drives.
///
/// `CARGO_BIN_EXE_*` covers `[[bin]]` targets only, and the helper is an
/// example on purpose: it is test scaffolding and has no business being
/// installable. `cargo test` builds examples, so it is there beside the test
/// binary's own directory.
#[cfg(unix)]
fn helper() -> PathBuf {
    let mut path = std::env::current_exe().expect("the test binary knows where it is");
    path.pop(); // deps/
    path.pop(); // debug/ or release/
    let path = path.join("examples").join("scratch_signal");
    assert!(
        path.exists(),
        "the helper example is missing at {}; `cargo test` builds it",
        path.display()
    );
    path
}

#[cfg(unix)]
#[test]
fn an_interrupted_process_removes_its_file() {
    let mut child = Command::new(helper())
        .stdout(Stdio::piped())
        .spawn()
        .expect("the helper example must be built alongside this test");

    let stdout = child.stdout.take().expect("stdout was piped");
    let mut line = String::new();
    BufReader::new(stdout)
        .read_line(&mut line)
        .expect("the helper prints the path it created");
    let path = PathBuf::from(line.trim());
    assert!(
        path.exists(),
        "the helper did not create {}",
        path.display()
    );

    // The genuine article: SIGINT, exactly as Ctrl+C delivers it.
    let signalled = Command::new("/bin/kill")
        .args(["-INT", &child.id().to_string()])
        .status()
        .expect("kill is a POSIX utility");
    assert!(signalled.success());

    let status = child.wait().expect("the helper must terminate");
    assert!(
        !status.success(),
        "an interrupted process must not report success: {status}"
    );

    // The signal thread does its work while the process is on its way out, so
    // the file may disappear a moment after the wait returns.
    let deadline = Instant::now() + Duration::from_secs(5);
    while path.exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(
        !path.exists(),
        "Ctrl+C left a copy of the attachment behind: {}",
        path.display()
    );
}
