//! What Clift will and will not accept as an attachment.
//!
//! Every refusal here is built from a real filesystem object -- a real FIFO, a
//! real socket, a real symbolic link -- rather than from a `FileKind` value
//! chosen by the test. The point is that the classification is right, and a
//! test that supplies its own classification cannot show that.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use clift_core::attachments::{inspect, inspect_all};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

static COUNTER: AtomicU32 = AtomicU32::new(0);

struct Scratch(PathBuf);

impl Scratch {
    fn new(label: &str) -> Self {
        let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "clift-attach-{}-{label}-{unique}",
            std::process::id()
        ));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }

    fn file(&self, name: &str, contents: &[u8]) -> PathBuf {
        let path = self.0.join(name);
        std::fs::write(&path, contents).unwrap();
        path
    }

    fn dir(&self, name: &str) -> PathBuf {
        let path = self.0.join(name);
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    fn link(&self, name: &str, target: &Path) -> PathBuf {
        let path = self.0.join(name);
        #[cfg(unix)]
        std::os::unix::fs::symlink(target, &path).unwrap();
        path
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn a_regular_file_is_accepted_with_its_size_and_name() {
    let scratch = Scratch::new("regular");
    let path = scratch.file("shot.png", b"0123456789");

    let attachment = inspect(&path).unwrap();
    assert_eq!(attachment.name().as_str(), "shot.png");
    assert_eq!(attachment.size(), 10);
    assert!(attachment.path().is_absolute());
}

/// Acceptance 5: names with spaces and non-ASCII characters survive.
#[test]
fn a_name_with_spaces_and_non_ascii_characters_survives() {
    let scratch = Scratch::new("names");
    for name in ["with spaces.txt", "名字.png", "emoji 🎯.txt"] {
        let path = scratch.file(name, b"x");
        let attachment = inspect(&path).unwrap();
        assert_eq!(attachment.name().as_str(), name, "{name}");
    }
}

/// Acceptance 2: a folder is refused with the reason and a way forward.
#[test]
fn a_directory_is_refused_with_an_archive_suggestion() {
    let scratch = Scratch::new("dir");
    let path = scratch.dir("pictures");

    let error = inspect(&path).expect_err("a folder is not an attachment");
    assert_eq!(error.exit_code().as_u8(), 24);
    assert!(error.to_string().contains("folder"), "{error}");
    assert!(
        error
            .remedy()
            .is_some_and(|remedy| remedy.command().contains("zip")),
        "the refusal must say what to do instead: {error}"
    );
}

/// Acceptance 3: a FIFO is refused rather than read. Reading one blocks until
/// something writes to it, which would hang the send with no explanation.
#[cfg(unix)]
#[test]
fn a_named_pipe_is_refused_rather_than_read() {
    let scratch = Scratch::new("fifo");
    let path = scratch.0.join("pipe");
    let status = std::process::Command::new("/usr/bin/mkfifo")
        .arg(&path)
        .status()
        .expect("mkfifo is a POSIX utility");
    assert!(status.success());

    let error = inspect(&path).expect_err("a pipe is not a file");
    assert_eq!(error.exit_code().as_u8(), 24);
    assert!(error.to_string().contains("named pipe"), "{error}");
}

/// Acceptance 3: a socket, likewise.
#[cfg(unix)]
#[test]
fn a_socket_is_refused() {
    let scratch = Scratch::new("socket");
    let path = scratch.0.join("sock");
    let listener = std::os::unix::net::UnixListener::bind(&path).unwrap();

    let error = inspect(&path).expect_err("a socket is not a file");
    assert_eq!(error.exit_code().as_u8(), 24);
    assert!(error.to_string().contains("socket"), "{error}");
    drop(listener);
}

/// A character device is refused too. `/dev/null` exists on every unix.
#[cfg(unix)]
#[test]
fn a_character_device_is_refused() {
    let error = inspect(Path::new("/dev/null")).expect_err("a device is not a file");
    assert_eq!(error.exit_code().as_u8(), 24);
    assert!(error.to_string().contains("character device"), "{error}");
}

/// Acceptance 4: a link to a regular file is followed; a link to a folder is
/// refused for being a folder.
#[cfg(unix)]
#[test]
fn a_symlink_is_resolved_to_what_it_points_at() {
    let scratch = Scratch::new("links");
    let target = scratch.file("real.png", b"abcd");
    let link = scratch.link("link.png", &target);

    let attachment = inspect(&link).unwrap();
    assert_eq!(attachment.size(), 4);
    assert_eq!(
        attachment.path().canonicalize().unwrap(),
        target.canonicalize().unwrap(),
        "the target is what gets uploaded"
    );
    assert_eq!(
        attachment.name().as_str(),
        "link.png",
        "the name the user was looking at is the one that is kept"
    );

    let directory = scratch.dir("folder");
    let to_folder = scratch.link("folder-link", &directory);
    let error = inspect(&to_folder).expect_err("a link to a folder is still a folder");
    assert!(error.to_string().contains("folder"), "{error}");
}

#[cfg(unix)]
#[test]
fn a_dangling_symlink_is_refused_with_a_way_to_look() {
    let scratch = Scratch::new("dangling");
    let link = scratch.link("gone.png", Path::new("/nonexistent/never/was"));

    let error = inspect(&link).expect_err("a link to nothing is not a file");
    assert_eq!(error.exit_code().as_u8(), 24);
    assert!(
        error
            .remedy()
            .is_some_and(|remedy| remedy.command().starts_with("ls -l")),
        "{error}"
    );
}

#[test]
fn a_path_that_does_not_exist_is_refused() {
    let error = inspect(Path::new("/nonexistent/never/was")).expect_err("nothing is there");
    assert_eq!(error.exit_code().as_u8(), 24);
}

/// A set is accepted whole or not at all: a partially accepted selection is a
/// selection the user did not make.
#[test]
fn one_bad_path_refuses_the_whole_set() {
    let scratch = Scratch::new("set");
    let good = scratch.file("a.png", b"a");
    let also_good = scratch.file("b.png", b"b");
    let bad = scratch.dir("folder");

    assert_eq!(
        inspect_all(&[good.clone(), also_good.clone()])
            .unwrap()
            .len(),
        2
    );
    assert!(inspect_all(&[good, bad, also_good]).is_err());
}

#[test]
fn an_empty_set_is_accepted_as_empty() {
    assert!(inspect_all(&[]).unwrap().is_empty());
}
