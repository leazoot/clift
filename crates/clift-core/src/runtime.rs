//! Clift's own private directory on the local machine.
//!
//! Everything Clift writes locally -- a clipboard image on its way out, the
//! small file `setup` uploads to prove the round trip works -- goes here rather
//! than into `/tmp`. A world-writable directory lets another account on the
//! same machine sit next to the file and play games with symbolic links, and
//! the contents of an attachment are exactly what must not be exposed that way
//!.
//!
//! Local filesystem access lives in `clift-core` by decision, not by accident:
//! only *remote* IO needs a port for testing, and a temporary directory is a
//! perfectly good double for the local one.

use crate::error::{CliftError, ErrorKind, Remedy, Stage};
use crate::places::{self, Platform};
use std::collections::BTreeSet;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

#[cfg(unix)]
const DIR_MODE: u32 = 0o700;
#[cfg(unix)]
const FILE_MODE: u32 = 0o600;

/// Clift's private working directory, created if it is missing.
///
/// Where it is comes from `places::run_dir`: the runtime directory where the
/// platform has one, otherwise the cache. A nomination that points into a
/// public directory is refused rather than followed.
///
/// # Errors
/// Fails when no usable base directory can be determined, and when the
/// directory cannot be created.
pub fn private_run_dir() -> Result<PathBuf, CliftError> {
    let directory = places::run_dir(Platform::current(), &places::process_environment)
        .map_err(|unlocated| {
            unlocated.into_error(Stage::Internal, ErrorKind::Internal, "a private directory")
        })?
        .join("run");
    ensure_private_dir(&directory)?;
    Ok(directory)
}

#[cfg(unix)]
fn ensure_private_dir(directory: &Path) -> Result<(), CliftError> {
    if directory.is_dir() {
        return Ok(());
    }
    fs::create_dir_all(directory).map_err(|error| dir_error(directory, error))?;
    fs::set_permissions(directory, fs::Permissions::from_mode(DIR_MODE))
        .map_err(|error| dir_error(directory, error))
}

#[cfg(not(unix))]
fn ensure_private_dir(directory: &Path) -> Result<(), CliftError> {
    // Windows uses ACLs rather than mode bits; tightening them is a v1.0 item.
    fs::create_dir_all(directory).map_err(|error| dir_error(directory, error))
}

fn dir_error(directory: &Path, error: std::io::Error) -> CliftError {
    CliftError::new(
        Stage::Internal,
        ErrorKind::Internal,
        format!("cannot prepare {}", directory.display()),
    )
    .with_source(error)
}

/// Every scratch file this process currently owns.
///
/// `Drop` handles the ordinary paths out of a function. This list handles the
/// one it cannot: a signal, which ends the process without unwinding.
fn live_files() -> &'static Mutex<BTreeSet<PathBuf>> {
    static LIVE: OnceLock<Mutex<BTreeSet<PathBuf>>> = OnceLock::new();
    LIVE.get_or_init(|| Mutex::new(BTreeSet::new()))
}

fn remember(path: &Path) {
    let mut live = live_files()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    live.insert(path.to_path_buf());
}

fn forget(path: &Path) {
    let mut live = live_files()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    live.remove(path);
}

/// Removes every scratch file this process still owns.
///
/// Called when a signal is about to end the process. Failures are ignored:
/// there is nobody left to tell, and a file that cannot be removed now will be
/// removed by the next run's cleanup.
pub fn remove_all_scratch_files() {
    let paths: Vec<PathBuf> = live_files()
        .lock()
        .map(|live| live.iter().cloned().collect())
        .unwrap_or_default();
    for path in paths {
        let _ = fs::remove_file(&path);
        forget(&path);
    }
}

/// Removes Clift's temporary files if the process is interrupted.
///
/// `Ctrl+C` ends a Rust program without unwinding, so no `Drop` runs and a
/// clipboard image would be left on disk. This starts a thread that waits for
/// the usual terminating signals, removes what is outstanding, and then lets
/// the signal do what it was going to do -- so the process still reports that
/// it was interrupted rather than that it exited.
///
/// Calling it more than once is harmless but pointless; the CLI calls it once,
/// at startup.
///
/// # Errors
/// Fails when the signals cannot be registered, which is not a reason to
/// refuse to run: the caller may log it and carry on with `Drop`-only cleanup.
#[cfg(unix)]
pub fn remove_scratch_files_on_signal() -> Result<(), CliftError> {
    use signal_hook::consts::{SIGHUP, SIGINT, SIGTERM};
    use signal_hook::iterator::Signals;

    let mut signals = Signals::new([SIGINT, SIGTERM, SIGHUP]).map_err(|error| {
        CliftError::new(
            Stage::Internal,
            ErrorKind::Internal,
            "could not listen for interrupt signals",
        )
        .with_source(error)
    })?;

    std::thread::Builder::new()
        .name("clift-signals".to_string())
        .spawn(move || {
            for signal in &mut signals {
                remove_all_scratch_files();
                // Restores the default disposition and re-raises, so the exit
                // status is the one a user pressing Ctrl+C expects.
                let _ = signal_hook::low_level::emulate_default_handler(signal);
            }
        })
        .map_err(|error| {
            CliftError::new(
                Stage::Internal,
                ErrorKind::Internal,
                "could not start the signal thread",
            )
            .with_source(error)
        })?;
    Ok(())
}

/// A private local file that removes itself when it goes out of scope.
///
/// The removal is in `Drop` rather than at each exit: normal return, an early
/// `?`, a panic and a caught signal are four different paths out of a function,
/// and only one of them is easy to remember.
#[derive(Debug)]
pub struct ScratchFile {
    path: PathBuf,
}

impl ScratchFile {
    /// Writes `contents` to a new private file inside [`private_run_dir`].
    ///
    /// `suffix` becomes the file's extension. It is not decoration: a clipboard
    /// image written as `.png` is recognisable to whatever the user opens it
    /// with if a send fails and they go looking for it.
    ///
    /// # Errors
    /// Fails when the directory cannot be prepared or the file cannot be
    /// written.
    pub fn create(prefix: &str, suffix: &str, contents: &[u8]) -> Result<Self, CliftError> {
        let directory = private_run_dir()?;
        // `create_new` in a loop rather than a random name: this file is not a
        // security boundary -- it holds a payload Clift just made up -- and the
        // loop cannot clobber an existing file either way.
        for attempt in 0..1_000u32 {
            let candidate = directory.join(format!(".{prefix}.{attempt}.{suffix}"));
            match new_private_file(&candidate) {
                Ok(mut file) => {
                    file.write_all(contents)
                        .and_then(|()| file.flush())
                        .map_err(|error| write_error(&candidate, error))?;
                    remember(&candidate);
                    return Ok(Self { path: candidate });
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(write_error(&candidate, error)),
            }
        }
        Err(CliftError::new(
            Stage::Internal,
            ErrorKind::Internal,
            format!(
                "cannot create a scratch file in {}: too many stale files",
                directory.display()
            ),
        )
        .with_remedy(Remedy::new(
            "Remove the leftovers, then retry:",
            match Platform::current() {
                Platform::Unix => format!("rm -f {}/.{prefix}.*", directory.display()),
                Platform::Windows => {
                    format!(
                        "Remove-Item -Force \"{}\\.{prefix}.*\"",
                        directory.display()
                    )
                }
            },
        )))
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }
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

fn write_error(path: &Path, error: std::io::Error) -> CliftError {
    CliftError::new(
        Stage::Internal,
        ErrorKind::Internal,
        format!("cannot write {}", path.display()),
    )
    .with_source(error)
}

impl Drop for ScratchFile {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
        forget(&self.path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_scratch_file_holds_its_contents_and_then_removes_itself() {
        let path;
        {
            let scratch = ScratchFile::create("test-payload", "bin", b"hello").unwrap();
            path = scratch.path().to_path_buf();
            assert_eq!(fs::read(&path).unwrap(), b"hello");
            assert!(!path.starts_with("/tmp"), "{}", path.display());
            assert_eq!(path.extension().and_then(|e| e.to_str()), Some("bin"));
        }
        assert!(
            !path.exists(),
            "the scratch file outlived its guard: {}",
            path.display()
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_scratch_file_is_readable_only_by_its_owner() {
        let scratch = ScratchFile::create("test-mode", "bin", b"x").unwrap();
        let mode = fs::metadata(scratch.path()).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, FILE_MODE);
    }

    #[test]
    fn two_scratch_files_do_not_share_a_name() {
        let first = ScratchFile::create("test-unique", "bin", b"a").unwrap();
        let second = ScratchFile::create("test-unique", "bin", b"b").unwrap();
        assert_ne!(first.path(), second.path());
        assert_eq!(fs::read(first.path()).unwrap(), b"a");
    }
}
