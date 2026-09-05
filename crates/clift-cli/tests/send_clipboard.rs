//! `clift send --clipboard` and `--copy`, with the real clipboard and a real
//! server.
//!
//! Needs both a container and permission to overwrite the clipboard of whoever
//! is running it:
//!
//! ```text
//! CLIFT_REAL_CLIPBOARD=1 cargo test -p clift-cli --test send_clipboard
//! ```

#![allow(clippy::unwrap_used, clippy::expect_used)]

#[path = "../../../tests/e2e/fixtures.rs"]
mod fixtures;

use fixtures::{SshdFixture, Topology, skip_without_docker};
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::{Mutex, MutexGuard};

/// One clipboard, one test at a time.
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

fn skip_clipboard(test: &str) -> bool {
    if std::env::var_os("CLIFT_REAL_CLIPBOARD").is_some() {
        return false;
    }
    announce(&format!(
        "SKIPPED {test}: CLIFT_REAL_CLIPBOARD is not set (it overwrites the \
         real clipboard); this test proved nothing"
    ));
    true
}

struct Sandbox {
    home: PathBuf,
    bin: PathBuf,
    work: PathBuf,
}

impl Sandbox {
    fn new(fixture: &SshdFixture, label: &str) -> Self {
        let home = fixture.workdir().join(format!("clip-home-{label}"));
        let bin = fixture.workdir().join(format!("clip-bin-{label}"));
        let work = fixture.workdir().join(format!("clip-work-{label}"));
        for path in [&home, &bin, &work] {
            fs::create_dir_all(path).unwrap();
        }
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

        let sandbox = Self { home, bin, work };
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

    fn file(&self, name: &str, contents: &[u8]) -> PathBuf {
        let path = self.work.join(name);
        fs::write(&path, contents).unwrap();
        path
    }
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

fn read_text() -> String {
    let output = Command::new("pbpaste").output().expect("pbpaste is macOS");
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn put_screenshot() {
    let status = Command::new("/usr/sbin/screencapture")
        .args(["-x", "-c"])
        .status()
        .unwrap();
    assert!(status.success());
}

fn clear_clipboard() {
    let output = Command::new("/usr/bin/osascript")
        .args([
            "-l",
            "JavaScript",
            "-e",
            "ObjC.import('AppKit'); $.NSPasteboard.generalPasteboard.clearContents; 'ok'",
        ])
        .output()
        .unwrap();
    assert!(output.status.success());
}

fn put_files(paths: &[&Path]) {
    const SCRIPT: &str = r"
        ObjC.import('AppKit');
        const args = $.NSProcessInfo.processInfo.arguments;
        const urls = $.NSMutableArray.array;
        for (let i = 5; i < args.count; i++) {
            urls.addObject($.NSURL.fileURLWithPath(args.objectAtIndex(i)));
        }
        const pb = $.NSPasteboard.generalPasteboard;
        pb.clearContents;
        if (!pb.writeObjects(urls)) { throw new Error('writeObjects refused the list'); }
        // Read the items back before this process exits. Without it the last
        // item was intermittently unreadable by the next process to look --
        // whatever the cause, a writer that has not seen its own items on the
        // pasteboard has not finished writing them, and a test that returns
        // there reports a fault in the reader that is not there.
        const items = pb.pasteboardItems;
        let readable = 0;
        for (let i = 0; i < items.count; i++) {
            if (!items.objectAtIndex(i).stringForType('public.file-url').isNil()) {
                readable++;
            }
        }
        String(readable);
    ";
    let mut command = Command::new("/usr/bin/osascript");
    command.args(["-l", "JavaScript", "-e", SCRIPT]);
    for path in paths {
        command.arg(path);
    }
    let output = command.output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        paths.len().to_string()
    );
}

fn stdout_of(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr_of(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn paths_in(text: &str) -> Vec<String> {
    let mut paths = Vec::new();
    for line in text.lines() {
        let quoted = line
            .strip_prefix("Please inspect this file: ")
            .or_else(|| line.strip_prefix("- "));
        if let Some(quoted) = quoted {
            let inner = quoted
                .strip_prefix('\'')
                .and_then(|rest| rest.strip_suffix('\''))
                .unwrap_or(quoted);
            paths.push(inner.replace("'\\''", "'"));
        }
    }
    paths
}

fn shell_quote(path: &str) -> String {
    format!("'{}'", path.replace('\'', "'\\''"))
}

fn remote(fixture: &SshdFixture, command: &str) -> String {
    String::from_utf8_lossy(&fixture.ssh(command).stdout)
        .trim()
        .to_string()
}

fn inbox_files(fixture: &SshdFixture) -> String {
    remote(
        fixture,
        &format!(
            "find \"{}/.cache/clift/inbox\" -type f",
            fixture.remote_home()
        ),
    )
}

/// A real screenshot, sent, arriving as a real PNG.
#[test]
fn a_screenshot_on_the_clipboard_arrives_as_a_png() {
    if skip_without_docker("a_screenshot_on_the_clipboard_arrives_as_a_png")
        || skip_clipboard("a_screenshot_on_the_clipboard_arrives_as_a_png")
    {
        return;
    }
    let _turn = exclusive();
    let fixture = SshdFixture::start(Topology::Normal);
    let sandbox = Sandbox::new(&fixture, "shot");
    put_screenshot();

    let output = sandbox.run(&["send", "--clipboard"]);
    assert!(output.status.success(), "{}", stderr_of(&output));

    let paths = paths_in(&stdout_of(&output));
    assert_eq!(paths.len(), 1, "{}", stdout_of(&output));
    assert!(paths[0].ends_with("/clipboard.png"), "{}", paths[0]);

    // The first eight bytes of a PNG, read on the far side.
    let magic = remote(
        &fixture,
        &format!("head -c 8 {} | od -An -tx1", shell_quote(&paths[0])),
    );
    assert_eq!(
        magic.split_whitespace().collect::<Vec<_>>().join(" "),
        "89 50 4e 47 0d 0a 1a 0a",
        "what arrived is not a PNG"
    );
    assert_eq!(
        remote(&fixture, &format!("stat -c %a {}", shell_quote(&paths[0]))),
        "600"
    );
}

/// Three files copied as Finder copies them, sent, all three arriving.
#[test]
fn three_copied_files_produce_three_paths() {
    if skip_without_docker("three_copied_files_produce_three_paths")
        || skip_clipboard("three_copied_files_produce_three_paths")
    {
        return;
    }
    let _turn = exclusive();
    let fixture = SshdFixture::start(Topology::Normal);
    let sandbox = Sandbox::new(&fixture, "three");

    let files: Vec<PathBuf> = (0..3)
        .map(|index| {
            sandbox.file(
                &format!("copied-{index}.txt"),
                format!("contents {index}").as_bytes(),
            )
        })
        .collect();
    put_files(&files.iter().map(PathBuf::as_path).collect::<Vec<_>>());

    let output = sandbox.run(&["send", "--clipboard"]);
    assert!(output.status.success(), "{}", stderr_of(&output));

    let paths = paths_in(&stdout_of(&output));
    assert_eq!(paths.len(), 3, "{}", stdout_of(&output));
    for (index, path) in paths.iter().enumerate() {
        assert_eq!(
            remote(&fixture, &format!("cat {}", shell_quote(path))),
            format!("contents {index}")
        );
    }
}

/// Plain text: exit code 10, and the host is never touched.
#[test]
fn plain_text_reports_nothing_to_send_and_uploads_nothing() {
    if skip_without_docker("plain_text_reports_nothing_to_send_and_uploads_nothing")
        || skip_clipboard("plain_text_reports_nothing_to_send_and_uploads_nothing")
    {
        return;
    }
    let _turn = exclusive();
    let fixture = SshdFixture::start(Topology::Normal);
    let sandbox = Sandbox::new(&fixture, "text");
    put_text("just some words");

    let output = sandbox.run(&["send", "--clipboard"]);
    assert_eq!(
        output.status.code(),
        Some(10),
        "plain text is exit code 10: {}",
        stderr_of(&output)
    );
    assert!(
        output.stdout.is_empty(),
        "nothing to paste, nothing on stdout"
    );
    assert!(inbox_files(&fixture).is_empty(), "text was uploaded");

    // And an empty clipboard says something different, with the same code.
    clear_clipboard();
    let output = sandbox.run(&["send", "--clipboard"]);
    assert_eq!(output.status.code(), Some(10));
    assert!(
        stderr_of(&output).contains("empty"),
        "{}",
        stderr_of(&output)
    );
    assert!(inbox_files(&fixture).is_empty());
}

/// `--copy` replaces the clipboard with the insertion text and says so.
#[test]
fn copy_replaces_the_clipboard_and_announces_it() {
    if skip_without_docker("copy_replaces_the_clipboard_and_announces_it")
        || skip_clipboard("copy_replaces_the_clipboard_and_announces_it")
    {
        return;
    }
    let _turn = exclusive();
    let fixture = SshdFixture::start(Topology::Normal);
    let sandbox = Sandbox::new(&fixture, "copy");
    let source = sandbox.file("note.txt", b"a note");
    put_text("whatever was here before");

    let output = sandbox.run(&["send", "--copy", source.to_str().unwrap()]);
    assert!(output.status.success(), "{}", stderr_of(&output));

    let text = stdout_of(&output);
    assert_eq!(
        read_text(),
        text.trim_end_matches('\n'),
        "the clipboard must hold exactly the insertion text"
    );
    let stderr = stderr_of(&output);
    assert!(
        stderr.contains("clipboard"),
        "the replacement must be announced: {stderr}"
    );
    assert!(
        !text.contains("clipboard now holds"),
        "the announcement must not be on stdout: {text}"
    );
}

/// A failed send leaves the clipboard exactly as it was.
#[test]
fn a_failed_send_leaves_the_clipboard_byte_for_byte_unchanged() {
    if skip_without_docker("a_failed_send_leaves_the_clipboard_byte_for_byte_unchanged")
        || skip_clipboard("a_failed_send_leaves_the_clipboard_byte_for_byte_unchanged")
    {
        return;
    }
    let _turn = exclusive();
    // The inbox is on a 1 MiB filesystem, so a 3 MiB attachment cannot arrive.
    let fixture = SshdFixture::start(Topology::SmallCache);
    let sandbox = Sandbox::new(&fixture, "copyfail");
    let source = sandbox.file("big.bin", &vec![b'x'; 3 * 1024 * 1024]);

    let before = "something the user copied and would rather keep";
    put_text(before);

    let output = sandbox.run(&["send", "--copy", source.to_str().unwrap()]);
    assert!(!output.status.success(), "3 MiB cannot fit in 1 MiB");
    assert!(output.stdout.is_empty(), "a failure produces no text");
    assert_eq!(
        read_text(),
        before,
        "a failed send replaced the user's clipboard"
    );
}
