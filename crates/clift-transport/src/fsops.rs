//! Remote directory operations, all of them over SFTP.
//!
//! Nothing here runs a remote shell. Every path is a field of an SFTP request
//! ([`crate::wire`]), and every piece of metadata is read from the attribute
//! block the server returns: the permission bits as a number, the modification
//! time as seconds since the epoch. There is no listing text to parse and no
//! time zone to guess.

use crate::errmap::{map_failure, map_refusal};
use crate::probe::OpenSshTransport;
use crate::session::{Failure, Refusal, SftpSession};
use crate::wire::{Attrs, status};
use clift_core::domain::{RemotePath, SafeFileName};
use clift_core::error::{CliftError, ErrorKind, Remedy, Stage};
use clift_core::ports::{ProbeReport, RemoteEntry, RemoteEntryKind, RemoteFs, TransportTarget};
use std::time::{Duration, UNIX_EPOCH};

const TYPE_MASK: u32 = 0o170_000;
const TYPE_DIRECTORY: u32 = 0o040_000;
const TYPE_FILE: u32 = 0o100_000;
const TYPE_SYMLINK: u32 = 0o120_000;

/// An operation's own verdict, or the session failure that prevented one.
///
/// The outer `Result` is the session: a broken one must reach the runner so
/// that it is dropped. The inner one is what the operation found, such as a
/// directory with the wrong permissions, which says nothing about the
/// session.
type Verdict<T> = Result<Result<T, CliftError>, Failure>;

impl OpenSshTransport {
    /// The remote home directory, as an absolute path.
    ///
    /// SFTP sessions start in the user's home, so the server's resolution of
    /// `.` is the answer. Asking a remote shell to echo `$HOME` would be both a
    /// shell invocation and a worse answer.
    ///
    /// # Errors
    /// Fails when the host cannot be reached or reports a path that is not
    /// absolute.
    pub fn resolve_home(&self, target: &TransportTarget) -> Result<RemotePath, CliftError> {
        let action = "could not read the remote home directory";
        let reported = self
            .runner()
            .sftp(target, |session| session.realpath("."))
            .map_err(|failure| {
                self.runner()
                    .session_error(target, Stage::Connect, action, failure)
            })?;
        let text = String::from_utf8(reported).map_err(|error| {
            CliftError::new(
                Stage::Connect,
                ErrorKind::RemoteDirectory,
                format!(
                    "{} reported a home directory that is not UTF-8",
                    target.ssh_host()
                ),
            )
            .with_source(error)
        })?;
        RemotePath::new(text).map_err(|error| {
            error
                .into_clift(Stage::Connect, ErrorKind::RemoteDirectory)
                .with_remedy(Remedy::new(
                    "Clift needs an absolute home directory. Check what the server reports:",
                    format!("ssh {} pwd", target.ssh_host()),
                ))
        })
    }

    /// The directory the host nominates for caches, if any.
    ///
    /// Read with a fixed literal command, which is why it may go through the
    /// login shell at all: there is no user input to interpolate. An unset
    /// variable makes `printenv` exit non-zero, which is a normal answer here
    /// rather than a failure.
    ///
    /// # Errors
    /// Fails when the host cannot be reached.
    pub fn resolve_cache_home(
        &self,
        target: &TransportTarget,
    ) -> Result<Option<RemotePath>, CliftError> {
        let outcome = self.runner().run_ssh(target, "printenv XDG_CACHE_HOME")?;
        if !outcome.succeeded() {
            // `printenv` exits 1 for an unset variable and prints nothing. Any
            // non-zero exit with no output means the same thing: the host told
            // us nothing, so there is nothing to respect.
            if outcome.stdout.trim().is_empty() {
                return Ok(None);
            }
            return Err(map_failure(
                target,
                Stage::Connect,
                "could not read XDG_CACHE_HOME",
                &outcome.stderr,
            ));
        }
        let value = outcome.stdout.trim();
        if value.is_empty() {
            return Ok(None);
        }
        // A relative or malformed value is not worth failing over; the caller
        // falls back to the home directory. Whether a valid location is one
        // Clift will use is a policy, and policies live in clift-core.
        Ok(RemotePath::new(value).ok())
    }

    /// Creates `path` with exactly `mode`, creating any missing parents with
    /// the same mode.
    ///
    /// An existing `path` is checked, never corrected: a directory that is
    /// already there with looser permissions is a situation the user has to
    /// see, not one Clift should quietly tighten behind their back. Ancestors
    /// that already exist are left exactly as they are, because they may well
    /// be ordinary directories such as `~/.cache` that have every right to be
    /// group readable.
    ///
    /// The directory is created with `mode` attached, and read back in the
    /// same round trip. The pair settles both questions at once: whether this
    /// call created it, and what its permissions are. A server applies its
    /// umask to the requested mode, which can only make it stricter; one that
    /// ignores the request altogether is caught by the read-back and corrected,
    /// because the directory is Clift's own.
    ///
    /// # Errors
    /// Fails when `path` exists as something other than a directory, when it
    /// exists with a different mode, or when the directory cannot be created.
    pub fn ensure_dir(
        &self,
        target: &TransportTarget,
        path: &RemotePath,
        mode: u32,
    ) -> Result<(), CliftError> {
        match self
            .runner()
            .sftp(target, |session| ensure_in(session, target, path, mode))
        {
            Ok(verdict) => verdict,
            Err(failure) => Err(self.runner().session_error(
                target,
                Stage::Staging,
                &format!("could not create {path}"),
                failure,
            )),
        }
    }

    /// Metadata for one path, or `None` when it does not exist.
    ///
    /// A symbolic link is reported as a link, never as what it points at.
    ///
    /// # Errors
    /// Fails when the path cannot be inspected for a reason other than not
    /// existing.
    pub fn stat(
        &self,
        target: &TransportTarget,
        path: &RemotePath,
    ) -> Result<Option<RemoteEntry>, CliftError> {
        if parent_of(path).is_none() {
            // Nothing in Clift needs to inspect `/`, and an entry for it would
            // need a name it does not have.
            return Err(CliftError::new(
                Stage::Staging,
                ErrorKind::RemoteDirectory,
                "the remote root directory cannot be inspected".to_string(),
            ));
        }
        // A name Clift cannot represent is reported as absent, as it is left
        // out of listings: it is exactly the kind of entry cleanup must not act
        // on.
        let Some(name) = base_name(path).and_then(|name| SafeFileName::new(name).ok()) else {
            return Ok(None);
        };
        let attrs = self
            .runner()
            .sftp(target, |session| session.lstat(path.as_str()))
            .map_err(|failure| {
                self.runner().session_error(
                    target,
                    Stage::Staging,
                    &format!("could not inspect {path}"),
                    failure,
                )
            })?;
        Ok(attrs.map(|attrs| entry(name, attrs)))
    }

    /// Lists a directory.
    ///
    /// Entries whose names Clift cannot represent safely are left out rather
    /// than approximated: an unrepresentable name is exactly the kind of thing
    /// cleanup must not act on.
    ///
    /// # Errors
    /// Fails when the directory cannot be listed.
    pub fn list_dir(
        &self,
        target: &TransportTarget,
        path: &RemotePath,
    ) -> Result<Vec<RemoteEntry>, CliftError> {
        let listed = self
            .runner()
            .sftp(target, |session| session.list(path.as_str()))
            .map_err(|failure| {
                self.runner().session_error(
                    target,
                    Stage::Staging,
                    &format!("could not list {path}"),
                    failure,
                )
            })?;
        let Some(names) = listed else {
            return Err(CliftError::new(
                Stage::Staging,
                ErrorKind::RemoteDirectory,
                format!("{path} does not exist on {}", target.ssh_host()),
            ));
        };
        Ok(names
            .into_iter()
            .filter_map(|listed| {
                let name = String::from_utf8(listed.filename).ok()?;
                if name == "." || name == ".." {
                    return None;
                }
                let safe = SafeFileName::new(name).ok()?;
                Some(entry(safe, listed.attrs))
            })
            .collect())
    }

    /// Removes a file, a symbolic link or an empty directory.
    ///
    /// A path that is already gone is success: cleanup runs repeatedly and
    /// concurrently, and treating an absent file as a failure would turn a
    /// normal race into an error the user has to read.
    ///
    /// A symbolic link is unlinked, never followed, which is what keeps
    /// cleanup inside the inbox even when someone has planted a link out of it.
    ///
    /// # Errors
    /// Fails when the path exists but cannot be removed.
    pub fn remove(&self, target: &TransportTarget, path: &RemotePath) -> Result<(), CliftError> {
        self.runner()
            .sftp(target, |session| {
                let Some(attrs) = session.lstat(path.as_str())? else {
                    return Ok(());
                };
                let removed = if kind_of(attrs) == RemoteEntryKind::Directory {
                    session.rmdir(path.as_str())
                } else {
                    session.remove(path.as_str())
                };
                match removed {
                    Err(Failure::Refused(refusal)) if refusal.code == status::NO_SUCH_FILE => {
                        Ok(())
                    }
                    other => other,
                }
            })
            .map_err(|failure| {
                self.runner().session_error(
                    target,
                    Stage::Staging,
                    &format!("could not remove {path}"),
                    failure,
                )
            })
    }
}

/// `ensure_dir` inside a session.
fn ensure_in(
    session: &mut SftpSession,
    target: &TransportTarget,
    path: &RemotePath,
    mode: u32,
) -> Verdict<()> {
    let (made, attrs) = session.mkdir_and_lstat(path.as_str(), mode)?;
    let missing_parent = matches!(&made, Err(refusal) if refusal.code == status::NO_SUCH_FILE);
    if !missing_parent {
        return settle(session, target, path, mode, made, attrs);
    }

    let Some(parent) = parent_of(path) else {
        return Ok(Err(CliftError::new(
            Stage::Staging,
            ErrorKind::RemoteDirectory,
            format!("{path} has no existing ancestor on {}", target.ssh_host()),
        )));
    };
    // Only the ancestors Clift creates are given `mode`; the ones already
    // there are checked and left alone. Each level asks the same question of
    // the same server, so that rule carries down without being restated.
    if let Err(wrong) = ensure_in(session, target, &parent, mode)? {
        return Ok(Err(wrong));
    }
    let (made, attrs) = session.mkdir_and_lstat(path.as_str(), mode)?;
    match made {
        // The parent is there now, so the same answer twice means something is
        // removing directories underneath us. That is reported, not chased.
        Err(refusal) if refusal.code == status::NO_SUCH_FILE => Ok(Err(CliftError::new(
            Stage::Staging,
            ErrorKind::RemoteDirectory,
            format!(
                "{path} still has no parent on {} after Clift created one",
                target.ssh_host()
            ),
        ))),
        made => settle(session, target, path, mode, made, attrs),
    }
}

/// Decides what a `mkdir` and the read-back after it mean.
fn settle(
    session: &mut SftpSession,
    target: &TransportTarget,
    path: &RemotePath,
    mode: u32,
    made: Result<(), Refusal>,
    attrs: Option<Attrs>,
) -> Verdict<()> {
    match (made, attrs) {
        (Err(refusal), _) if refusal.code == status::PERMISSION_DENIED => Ok(Err(refused(
            target,
            &format!("could not create {path}"),
            &refusal,
        ))),
        // Created by this call, so its permissions are Clift's to set.
        (Ok(()), Some(attrs)) if kind_of(attrs) == RemoteEntryKind::Directory => {
            if permissions(attrs) == Some(mode) {
                return Ok(Ok(()));
            }
            match session.set_mode(path.as_str(), mode) {
                Ok(()) => {}
                Err(Failure::Refused(refusal)) => {
                    return Ok(Err(refused(
                        target,
                        &format!("could not set the permissions of {path}"),
                        &refusal,
                    )));
                }
                Err(other) => return Err(other),
            }
            Ok(match session.lstat(path.as_str())? {
                Some(after) => check_existing(target, path, mode, after),
                None => Err(vanished(path)),
            })
        }
        (Ok(()), Some(attrs)) => Ok(check_existing(target, path, mode, attrs)),
        (Ok(()), None) => Ok(Err(vanished(path))),
        // Nothing is there, so the refusal was not "it already exists".
        // Whatever the server actually said is the answer.
        (Err(refusal), None) => Ok(Err(refused(
            target,
            &format!("could not create {path}"),
            &refusal,
        ))),
        // Already there, and not Clift's to change.
        (Err(_), Some(attrs)) => match check_existing(target, path, mode, attrs) {
            Ok(()) => Ok(Ok(())),
            // A directory Clift did not create may be one another Clift has
            // only just made, on a server that ignores the mode sent with
            // `mkdir` and so needs a second request to set it. One more look,
            // on a path that is already failing, tells that moment apart from
            // a directory that is genuinely wrong.
            Err(wrong) => Ok(match session.lstat(path.as_str())? {
                Some(again) => check_existing(target, path, mode, again),
                None => Err(wrong),
            }),
        },
    }
}

fn check_existing(
    target: &TransportTarget,
    path: &RemotePath,
    mode: u32,
    existing: Attrs,
) -> Result<(), CliftError> {
    if kind_of(existing) != RemoteEntryKind::Directory {
        return Err(CliftError::new(
            Stage::Staging,
            ErrorKind::RemoteDirectory,
            format!("{path} on {} is not a directory", target.ssh_host()),
        ));
    }
    match permissions(existing) {
        Some(actual) if actual == mode => Ok(()),
        Some(actual) => Err(CliftError::new(
            Stage::Staging,
            ErrorKind::RemoteDirectory,
            format!(
                "{path} on {} has mode {actual:04o}, but Clift requires {mode:04o}",
                target.ssh_host()
            ),
        )
        .with_remedy(Remedy::new(
            "Clift does not change permissions on a directory you already have. \
             Set them yourself if that is what you want:",
            format!("ssh {} chmod {mode:o} {path}", target.ssh_host()),
        ))),
        None => Err(CliftError::new(
            Stage::Staging,
            ErrorKind::RemoteDirectory,
            format!(
                "{} did not report the permissions of {path}, so Clift cannot confirm they are \
                 {mode:04o}",
                target.ssh_host()
            ),
        )),
    }
}

fn refused(target: &TransportTarget, action: &str, refusal: &Refusal) -> CliftError {
    map_refusal(
        target,
        Stage::Staging,
        action,
        refusal.code,
        &refusal.message,
    )
}

fn vanished(path: &RemotePath) -> CliftError {
    CliftError::new(
        Stage::Staging,
        ErrorKind::RemoteDirectory,
        format!("{path} was reported as created but is not there"),
    )
}

/// The permission bits, without the file type or the setuid, setgid and
/// sticky bits: a directory inheriting setgid from its parent is still private
/// when its owner, group and other bits say so.
fn permissions(attrs: Attrs) -> Option<u32> {
    attrs.permissions.map(|bits| bits & 0o777)
}

fn kind_of(attrs: Attrs) -> RemoteEntryKind {
    match attrs.permissions.map(|bits| bits & TYPE_MASK) {
        Some(TYPE_DIRECTORY) => RemoteEntryKind::Directory,
        Some(TYPE_FILE) => RemoteEntryKind::File,
        Some(TYPE_SYMLINK) => RemoteEntryKind::Symlink,
        _ => RemoteEntryKind::Other,
    }
}

fn entry(name: SafeFileName, attrs: Attrs) -> RemoteEntry {
    RemoteEntry {
        name,
        kind: kind_of(attrs),
        size: attrs.size.unwrap_or(0),
        mode: permissions(attrs),
        modified: attrs
            .mtime
            .map(|seconds| UNIX_EPOCH + Duration::from_secs(u64::from(seconds))),
    }
}

/// The parent of an absolute path, or `None` for the root.
fn parent_of(path: &RemotePath) -> Option<RemotePath> {
    let text = path.as_str();
    if text == "/" {
        return None;
    }
    let cut = text.rfind('/')?;
    let parent = if cut == 0 { "/" } else { &text[..cut] };
    RemotePath::new(parent).ok()
}

fn base_name(path: &RemotePath) -> Option<&str> {
    let text = path.as_str();
    let cut = text.rfind('/')?;
    let name = &text[cut + 1..];
    if name.is_empty() { None } else { Some(name) }
}

/// The port implementation, delegating to the inherent methods above.
///
/// Kept as one block so the trait and its implementation cannot drift apart:
/// adding a method to `RemoteFs` stops compiling here until it is written.
impl RemoteFs for OpenSshTransport {
    fn probe(&self, target: &TransportTarget) -> Result<ProbeReport, CliftError> {
        OpenSshTransport::probe(self, target)
    }

    fn resolve_home(&self, target: &TransportTarget) -> Result<RemotePath, CliftError> {
        OpenSshTransport::resolve_home(self, target)
    }

    fn resolve_cache_home(
        &self,
        target: &TransportTarget,
    ) -> Result<Option<RemotePath>, CliftError> {
        OpenSshTransport::resolve_cache_home(self, target)
    }

    fn ensure_dir(
        &self,
        target: &TransportTarget,
        path: &RemotePath,
        mode: u32,
    ) -> Result<(), CliftError> {
        OpenSshTransport::ensure_dir(self, target, path, mode)
    }

    fn stat(
        &self,
        target: &TransportTarget,
        path: &RemotePath,
    ) -> Result<Option<RemoteEntry>, CliftError> {
        OpenSshTransport::stat(self, target, path)
    }

    fn list_dir(
        &self,
        target: &TransportTarget,
        path: &RemotePath,
    ) -> Result<Vec<RemoteEntry>, CliftError> {
        OpenSshTransport::list_dir(self, target, path)
    }

    fn remove(&self, target: &TransportTarget, path: &RemotePath) -> Result<(), CliftError> {
        OpenSshTransport::remove(self, target, path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target() -> TransportTarget {
        TransportTarget::new("dev126")
    }

    fn attrs(bits: u32) -> Attrs {
        Attrs {
            size: Some(4096),
            permissions: Some(bits),
            mtime: Some(1_788_000_000),
        }
    }

    #[test]
    fn a_parent_is_the_path_without_its_last_component() {
        let path = RemotePath::new("/home/dev/.cache/clift").unwrap();
        assert_eq!(
            parent_of(&path).map(|parent| parent.as_str().to_string()),
            Some("/home/dev/.cache".to_string())
        );
        assert_eq!(base_name(&path), Some("clift"));
        assert_eq!(
            parent_of(&RemotePath::new("/home").unwrap()).map(|p| p.as_str().to_string()),
            Some("/".to_string())
        );
        assert_eq!(
            parent_of(&RemotePath::new("/").unwrap()),
            None,
            "the root has no parent to climb to"
        );
    }

    #[test]
    fn the_type_and_permission_bits_are_read_from_the_mode_number() {
        assert_eq!(kind_of(attrs(0o040_700)), RemoteEntryKind::Directory);
        assert_eq!(kind_of(attrs(0o100_600)), RemoteEntryKind::File);
        assert_eq!(kind_of(attrs(0o120_777)), RemoteEntryKind::Symlink);
        assert_eq!(kind_of(attrs(0o010_644)), RemoteEntryKind::Other);
        assert_eq!(kind_of(Attrs::default()), RemoteEntryKind::Other);
        assert_eq!(permissions(attrs(0o042_700)), Some(0o700));
    }

    #[test]
    fn an_entry_carries_an_absolute_modification_time() {
        let listed = entry(SafeFileName::new("shot.png").unwrap(), attrs(0o100_600));
        assert_eq!(listed.kind, RemoteEntryKind::File);
        assert_eq!(listed.mode, Some(0o600));
        assert_eq!(listed.size, 4096);
        assert_eq!(
            listed
                .modified
                .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
                .map(|since| since.as_secs()),
            Some(1_788_000_000)
        );
    }

    #[test]
    fn an_existing_directory_is_held_to_every_permission_bit() {
        let path = RemotePath::new("/root/.cache/clift").unwrap();
        assert!(check_existing(&target(), &path, 0o700, attrs(0o040_700)).is_ok());
        assert!(
            check_existing(&target(), &path, 0o700, attrs(0o042_700)).is_ok(),
            "setgid inherited from a parent does not make a directory less private"
        );

        let loose = check_existing(&target(), &path, 0o700, attrs(0o040_755)).unwrap_err();
        assert_eq!(loose.exit_code().as_u8(), 25);
        assert!(loose.message().contains("0755"), "{loose}");
        assert!(loose.remedy().is_some());

        let file = check_existing(&target(), &path, 0o700, attrs(0o100_700)).unwrap_err();
        assert!(file.message().contains("not a directory"), "{file}");

        let unreported = Attrs::default();
        assert!(check_existing(&target(), &path, 0o700, unreported).is_err());
    }
}
