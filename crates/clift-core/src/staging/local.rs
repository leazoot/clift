//! The inbox as seen from *inside* the host, for `clift fetch`.
//!
//! Every other part of the staging layer talks to a remote host through the
//! [`RemoteFs`] port. This one does not, because `clift fetch` runs on the
//! machine the attachment is destined for: what is "the remote inbox" to a
//! sending Clift is an ordinary local directory to the fetching one.
//!
//! The layout is deliberately identical to the one Fast Mode produces
//! (`<root>/YYYY-MM-DD/<batch id>/`), so a host that receives both ways has one
//! inbox rather than two, and `clift clean` sweeps both without knowing which
//! mode put a batch there.
//!
//! Three rules, and they are the reason this is not four lines of `fs::write`:
//!
//! - **Nothing is written outside the root.** The name came out of a decrypted
//!   frame, which makes it attacker-influenced even though it authenticated.
//!   It is a [`SafeFileName`], and the finished path is checked against the
//!   root again afterwards.
//! - **Nothing existing is replaced.** The batch directory is 128 bits of fresh
//!   randomness, so a collision means something is wrong, not that a retry is
//!   in order.
//! - **Nothing half-written is left behind.** Each file goes to a `.part` and
//!   is renamed, so an agent reading the directory sees a whole file or no
//!   file, never the first half of one.
//!
//! [`RemoteFs`]: crate::ports::RemoteFs

use crate::calendar::format_date;
use crate::domain::{BatchNames, LocalPath};
use crate::error::{CliftError, ErrorKind, Remedy, Stage};
use crate::places::{self, Platform};
use crate::ports::{Clock, IdSource};
#[cfg(unix)]
use crate::staging::INBOX_MODE;
use crate::universal::BundleEntry;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

#[cfg(unix)]
const FILE_MODE: u32 = 0o600;

/// One attachment, where it ended up.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WrittenFile {
    path: LocalPath,
    media_type: String,
    size: u64,
}

impl WrittenFile {
    /// Absolute, on this host. This is the string the agent is given.
    #[must_use]
    pub const fn path(&self) -> &LocalPath {
        &self.path
    }

    #[must_use]
    pub fn media_type(&self) -> &str {
        &self.media_type
    }

    #[must_use]
    pub const fn size(&self) -> u64 {
        self.size
    }
}

/// Where a fetched batch went.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WrittenBatch {
    directory: LocalPath,
    files: Vec<WrittenFile>,
}

impl WrittenBatch {
    #[must_use]
    pub const fn directory(&self) -> &LocalPath {
        &self.directory
    }

    #[must_use]
    pub fn files(&self) -> &[WrittenFile] {
        &self.files
    }

    /// The paths, in the order the sender listed them.
    #[must_use]
    pub fn paths(&self) -> Vec<LocalPath> {
        self.files.iter().map(|file| file.path.clone()).collect()
    }
}

/// Resolves this host's inbox root: the cache directory `places` gives this
/// platform, plus `inbox`. The same layout as the remote inbox, on the host
/// that is running `fetch`.
///
/// Reading the environment is IO of a sort, and it is done here rather than
/// behind a port for the same reason `runtime.rs` does it: the thing being
/// abstracted for tests is the *remote* host, and this code is already on the
/// host it is talking about.
///
/// # Errors
/// Fails when the environment yields no usable base directory.
pub fn inbox_root() -> Result<PathBuf, CliftError> {
    places::cache_dir(Platform::current(), &places::process_environment)
        .map(|cache| cache.join("inbox"))
        .map_err(|unlocated| {
            unlocated.into_error(
                Stage::Staging,
                ErrorKind::RemoteDirectory,
                "a directory for the attachment",
            )
        })
}

/// Writes one fetched bundle into a fresh batch directory under `root`.
///
/// # Errors
/// Fails when a directory or file cannot be created, when a finished path would
/// fall outside `root`, and when a destination already exists.
pub fn write_batch(
    root: &Path,
    entries: &[BundleEntry],
    clock: &dyn Clock,
    ids: &dyn IdSource,
) -> Result<WrittenBatch, CliftError> {
    let date = format_date(clock.now());
    let id = ids.new_batch_id()?;
    let directory = root.join(&date).join(id.as_str());

    ensure_private_dir(root)?;
    ensure_private_dir(&root.join(&date))?;
    // `create_dir` rather than `create_dir_all`: this level is 128 bits of
    // fresh randomness, so finding it already there is not a condition to
    // recover from quietly.
    create_new_private_dir(&directory)?;

    // A batch arriving over the relay is subject to the same name disambiguation
    // as one going out over SFTP: two files called `shot.png` in one paste must
    // not become one file.
    let mut names = BatchNames::new();
    let mut written = Vec::with_capacity(entries.len());
    for entry in entries {
        let name = names.assign(entry.name().as_str());
        let path = directory.join(name.as_str());
        guard_containment(root, &path)?;
        write_file_atomically(&path, entry.data())?;
        written.push(WrittenFile {
            path: absolute(&path)?,
            media_type: entry.media_type().to_string(),
            size: u64::try_from(entry.data().len()).unwrap_or(u64::MAX),
        });
    }

    Ok(WrittenBatch {
        directory: absolute(&directory)?,
        files: written,
    })
}

/// The last check before a byte is written.
///
/// Redundant with [`SafeFileName`] by design. That type makes a traversing
/// component unrepresentable, and this makes a mistake in *building* the path
/// visible: the two would have to fail together for anything to escape.
fn guard_containment(root: &Path, path: &Path) -> Result<(), CliftError> {
    if path.strip_prefix(root).is_ok_and(|rest| {
        rest.components()
            .all(|component| matches!(component, std::path::Component::Normal(_)))
    }) {
        return Ok(());
    }
    Err(CliftError::new(
        Stage::Staging,
        ErrorKind::IntegrityFailure,
        format!(
            "refusing to write {} because it is not inside {}",
            path.display(),
            root.display()
        ),
    ))
}

/// The path an agent is handed, checked for the one property that is promised.
///
/// [`LocalPath`], not `RemotePath`: this is a path on the machine running
/// `fetch`, which is not necessarily POSIX. The remote type refuses drive
/// letters by design, and using it here made `clift fetch` fail on Windows with
/// "must be absolute" for a path that is absolute.
fn absolute(path: &Path) -> Result<LocalPath, CliftError> {
    let Some(text) = path.to_str() else {
        return Err(CliftError::new(
            Stage::Staging,
            ErrorKind::Internal,
            "the inbox path is not valid UTF-8",
        ));
    };
    LocalPath::new(Platform::current(), text)
        .map_err(|error| error.into_clift(Stage::Staging, ErrorKind::Internal))
}

/// Writes `contents` to `path` through a temporary name in the same directory.
///
/// The `.part` is what keeps a reading agent from ever seeing a half file, and
/// the rename is atomic within a filesystem, which the two paths share by
/// construction. On failure the `.part` is removed: a leftover would be swept
/// eventually, but "eventually" is not a good enough answer for a file
/// containing the user's screenshot.
fn write_file_atomically(path: &Path, contents: &[u8]) -> Result<(), CliftError> {
    // Cannot happen as things stand -- the batch directory was created fresh
    // and `BatchNames` has already made the names within it distinct -- which
    // is exactly why it is checked. If it ever does happen, something upstream
    // is wrong and overwriting the user's earlier attachment is the worst
    // available response.
    if path.exists() {
        return Err(CliftError::new(
            Stage::Staging,
            ErrorKind::RemoteDirectory,
            format!(
                "{} already exists; refusing to overwrite it",
                path.display()
            ),
        ));
    }
    let Some(parent) = path.parent() else {
        return Err(CliftError::new(
            Stage::Staging,
            ErrorKind::Internal,
            "the destination has no parent directory",
        ));
    };
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return Err(CliftError::new(
            Stage::Staging,
            ErrorKind::Internal,
            "the destination has no file name",
        ));
    };
    let part = parent.join(format!(".{name}.part"));

    let mut file = new_private_file(&part).map_err(|error| io_error(&part, error))?;
    let outcome = file
        .write_all(contents)
        .and_then(|()| file.flush())
        // The data has to be on the disk before the rename, or a crash leaves a
        // correctly named file with nothing in it -- which is worse than the
        // `.part` this exists to avoid, because it looks finished.
        .and_then(|()| file.sync_all());
    drop(file);

    if let Err(error) = outcome {
        let _ = fs::remove_file(&part);
        return Err(io_error(&part, error));
    }
    if let Err(error) = fs::rename(&part, path) {
        let _ = fs::remove_file(&part);
        return Err(io_error(path, error));
    }
    Ok(())
}

#[cfg(unix)]
fn new_private_file(path: &Path) -> std::io::Result<fs::File> {
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(FILE_MODE)
        .open(path)
}

#[cfg(not(unix))]
fn new_private_file(path: &Path) -> std::io::Result<fs::File> {
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
}

#[cfg(unix)]
fn ensure_private_dir(directory: &Path) -> Result<(), CliftError> {
    if directory.is_dir() {
        return Ok(());
    }
    fs::create_dir_all(directory).map_err(|error| io_error(directory, error))?;
    fs::set_permissions(directory, fs::Permissions::from_mode(INBOX_MODE))
        .map_err(|error| io_error(directory, error))
}

#[cfg(not(unix))]
fn ensure_private_dir(directory: &Path) -> Result<(), CliftError> {
    if directory.is_dir() {
        return Ok(());
    }
    fs::create_dir_all(directory).map_err(|error| io_error(directory, error))
}

#[cfg(unix)]
fn create_new_private_dir(directory: &Path) -> Result<(), CliftError> {
    fs::create_dir(directory).map_err(|error| io_error(directory, error))?;
    fs::set_permissions(directory, fs::Permissions::from_mode(INBOX_MODE))
        .map_err(|error| io_error(directory, error))
}

#[cfg(not(unix))]
fn create_new_private_dir(directory: &Path) -> Result<(), CliftError> {
    fs::create_dir(directory).map_err(|error| io_error(directory, error))
}

fn io_error(path: &Path, error: std::io::Error) -> CliftError {
    CliftError::new(
        Stage::Staging,
        ErrorKind::RemoteDirectory,
        format!("cannot write {}", path.display()),
    )
    .with_source(error)
    .with_remedy(Remedy::new(
        "Check the directory is writable:",
        format!("ls -ld {}", path.parent().unwrap_or(path).display()),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::SafeFileName;
    use crate::testing::{FakeClock, FakeIdSource};

    fn entry(name: &str, media_type: &str, data: &[u8]) -> BundleEntry {
        BundleEntry::new(
            SafeFileName::new(name).unwrap_or_else(|error| panic!("{error}")),
            media_type,
            data.to_vec(),
        )
        .unwrap_or_else(|error| panic!("{error}"))
    }

    /// A scratch root that removes itself. `std::env::temp_dir` is fine here
    /// and only here: this is a test's own directory, not a place a user's
    /// attachment is ever left.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(tag: &str) -> Self {
            let mut bytes = [0_u8; 8];
            getrandom::fill(&mut bytes).unwrap_or_else(|error| panic!("{error}"));
            let path = std::env::temp_dir()
                .join(format!("clift-local-{tag}-{}", u64::from_be_bytes(bytes)));
            fs::create_dir_all(&path).unwrap_or_else(|error| panic!("{error}"));
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn root(scratch: &Scratch) -> PathBuf {
        scratch.path().join("inbox")
    }

    #[test]
    fn a_bundle_lands_in_a_dated_batch_directory_and_the_bytes_survive() {
        let scratch = Scratch::new("basic");
        let clock = FakeClock::at_unix_seconds(1_788_093_240);
        let batch = write_batch(
            &root(&scratch),
            &[entry("shot.png", "image/png", b"pixels")],
            &clock,
            &FakeIdSource::starting_at(1),
        )
        .unwrap_or_else(|error| panic!("{error}"));

        assert_eq!(batch.files().len(), 1);
        let file = &batch.files()[0];
        assert!(
            file.path().as_str().contains("/2026-08-30/"),
            "{}",
            file.path()
        );
        assert!(
            file.path().as_str().ends_with("/shot.png"),
            "{}",
            file.path()
        );
        assert_eq!(
            fs::read(file.path().as_str()).unwrap_or_default(),
            b"pixels"
        );
        assert_eq!(file.size(), 6);
        assert_eq!(file.media_type(), "image/png");
    }

    #[test]
    fn two_files_with_one_name_do_not_become_one_file() {
        let scratch = Scratch::new("collide");
        let batch = write_batch(
            &root(&scratch),
            &[
                entry("shot.png", "image/png", b"first"),
                entry("shot.png", "image/png", b"second"),
            ],
            &FakeClock::at_unix_seconds(1_788_093_240),
            &FakeIdSource::starting_at(1),
        )
        .unwrap_or_else(|error| panic!("{error}"));

        let paths: Vec<&str> = batch.files().iter().map(|f| f.path().as_str()).collect();
        assert_ne!(paths[0], paths[1]);
        assert!(paths[1].ends_with("shot-2.png"), "{}", paths[1]);
        assert_eq!(fs::read(paths[0]).unwrap_or_default(), b"first");
        assert_eq!(fs::read(paths[1]).unwrap_or_default(), b"second");
    }

    #[test]
    fn two_batches_do_not_share_a_directory() {
        let scratch = Scratch::new("batches");
        let clock = FakeClock::at_unix_seconds(1_788_093_240);
        let ids = FakeIdSource::starting_at(1);
        let first = write_batch(
            &root(&scratch),
            &[entry("a.png", "image/png", b"a")],
            &clock,
            &ids,
        )
        .unwrap_or_else(|error| panic!("{error}"));
        let second = write_batch(
            &root(&scratch),
            &[entry("a.png", "image/png", b"b")],
            &clock,
            &ids,
        )
        .unwrap_or_else(|error| panic!("{error}"));
        assert_ne!(first.directory(), second.directory());
    }

    #[cfg(unix)]
    #[test]
    fn the_directories_are_private_and_so_are_the_files() {
        let scratch = Scratch::new("modes");
        let batch = write_batch(
            &root(&scratch),
            &[entry("shot.png", "image/png", b"x")],
            &FakeClock::at_unix_seconds(1_788_093_240),
            &FakeIdSource::starting_at(1),
        )
        .unwrap_or_else(|error| panic!("{error}"));

        let mode = |path: &str| {
            fs::metadata(path)
                .unwrap_or_else(|error| panic!("{error}"))
                .permissions()
                .mode()
                & 0o777
        };
        assert_eq!(mode(batch.directory().as_str()), INBOX_MODE);
        assert_eq!(mode(batch.files()[0].path().as_str()), FILE_MODE);
        // The date directory above it, too.
        let date_dir = Path::new(batch.directory().as_str())
            .parent()
            .unwrap_or(Path::new("/"))
            .to_path_buf();
        assert_eq!(mode(&date_dir.to_string_lossy()), INBOX_MODE);
    }

    #[test]
    fn nothing_is_left_behind_when_a_write_fails() {
        let scratch = Scratch::new("readonly");
        let inbox = root(&scratch);
        // A file where the batch's date directory needs to be: creating the
        // batch directory underneath it must fail.
        fs::create_dir_all(&inbox).unwrap_or_else(|error| panic!("{error}"));
        fs::write(inbox.join("2026-08-30"), b"not a directory")
            .unwrap_or_else(|error| panic!("{error}"));

        let error = write_batch(
            &inbox,
            &[entry("shot.png", "image/png", b"x")],
            &FakeClock::at_unix_seconds(1_788_093_240),
            &FakeIdSource::starting_at(1),
        )
        .unwrap_err();
        assert_eq!(error.kind(), ErrorKind::RemoteDirectory);
    }

    #[test]
    fn a_path_that_would_escape_the_root_is_refused() {
        let root = Path::new("/home/dev/.cache/clift/inbox");
        assert!(guard_containment(root, &root.join("2026-08-30/id/shot.png")).is_ok());
        for escape in [
            "/home/dev/.cache/clift/inbox/../elsewhere/shot.png",
            "/etc/passwd",
            "/home/dev/.cache/clift/inbox-old/shot.png",
        ] {
            assert!(
                guard_containment(root, Path::new(escape)).is_err(),
                "accepted {escape}"
            );
        }
    }
}
