//! Remote directory operations, all of them over SFTP.
//!
//! Nothing here runs a remote shell. The specification requires paths to be passed as
//! protocol fields rather than pasted into a command line, so every operation
//! is an SFTP verb with quoted operands; `sftp` parses the batch locally and
//! the server never sees the text.
//!
//! The one awkward part is reading metadata back. SFTP's client offers no
//! `stat` command, only `ls -l`, so a single path is inspected by listing its
//! parent. Modification times are rendered by the local `strftime`, which is
//! why the runner pins the `sftp` child to UTC; see `SshRunner::run_sftp`.

use crate::errmap::{Symptom, classify, map_failure};
use crate::probe::OpenSshTransport;
use crate::proc::SftpBatch;
use clift_core::calendar::{civil_from_days, days_from_civil, unix_seconds};
use clift_core::domain::{RemotePath, SafeFileName};
use clift_core::error::{CliftError, ErrorKind, Remedy, Stage};
use clift_core::ports::{ProbeReport, RemoteEntry, RemoteEntryKind, RemoteFs, TransportTarget};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

impl OpenSshTransport {
    /// The remote home directory, as an absolute path.
    ///
    /// SFTP sessions start in the user's home, so its own idea of the working
    /// directory is the answer. Asking a remote shell to echo `$HOME` would be
    /// both a shell invocation and a worse answer.
    ///
    /// # Errors
    /// Fails when the host cannot be reached or reports a path that is not
    /// absolute.
    pub fn resolve_home(&self, target: &TransportTarget) -> Result<RemotePath, CliftError> {
        let mut batch = SftpBatch::new();
        batch.push("pwd", &[])?;
        let outcome = self.runner().run_sftp(target, &batch)?;
        if !outcome.succeeded() {
            return Err(sftp_failed(
                target,
                "could not read the remote working directory",
                &outcome.stderr,
            ));
        }

        let reported = outcome
            .stdout
            .lines()
            .find_map(|line| line.split_once("Remote working directory:"))
            .map(|(_, path)| path.trim())
            .ok_or_else(|| {
                CliftError::new(
                    Stage::Connect,
                    ErrorKind::SshConnection,
                    format!(
                        "{} did not report a remote working directory",
                        target.ssh_host()
                    ),
                )
            })?;

        RemotePath::new(reported).map_err(|error| {
            error
                .into_clift(Stage::Connect, ErrorKind::RemoteDirectory)
                .with_remedy(Remedy::new(
                    "Clift needs an absolute home directory. Check what the server reports:",
                    format!("sftp {} <<< pwd", target.ssh_host()),
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
    /// `mkdir` is asked before anything else, and its refusal carries the
    /// answer that used to cost a `stat`: one request instead of the five a
    /// directory listing costs, and it is the request that had to be made
    /// anyway. It also settles the race that used to need a retry -- two
    /// batches started on the same day share their date directory, and
    /// whichever loses now gets a normal answer rather than an error
    /// indistinguishable from a real one.
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
        let refusal = self.make_dir(target, path, mode)?;
        match self.confirm_mode(target, path, mode, refusal.as_deref()) {
            Ok(()) => Ok(()),
            // A directory Clift did not create can be caught in the moment
            // between another Clift's `mkdir` and its `chmod`, still wearing
            // whatever the remote umask made it. The sftp client cannot create
            // a directory and its permissions in one request, so that window
            // is real, and looking once more is the only way to tell it from a
            // directory that is genuinely wrong. One extra look, on a path
            // that is already failing, and only for a directory Clift did not
            // create -- one it did create has no such excuse.
            Err(_maybe_still_settling) if refusal.is_some() => {
                self.confirm_mode(target, path, mode, refusal.as_deref())
            }
            Err(wrong) => Err(wrong),
        }
    }

    /// Creates `path` and any missing ancestors.
    ///
    /// Returns the server's own words when it declined to create `path`. That
    /// usually means the directory is already there -- SFTP protocol 3 has no
    /// distinct code for it -- but a full disk is worded the same way, so the
    /// text is carried up rather than interpreted here.
    fn make_dir(
        &self,
        target: &TransportTarget,
        path: &RemotePath,
        mode: u32,
    ) -> Result<Option<String>, CliftError> {
        match self.mkdir(target, path)? {
            Made::Created => {
                self.set_mode(target, path, mode)?;
                Ok(None)
            }
            Made::Refused(reason) => Ok(Some(reason)),
            Made::ParentMissing => {
                let Some(parent) = parent_of(path) else {
                    return Err(CliftError::new(
                        Stage::Staging,
                        ErrorKind::RemoteDirectory,
                        format!("{path} has no existing ancestor on {}", target.ssh_host()),
                    ));
                };
                // Only the ancestors Clift creates are given `mode`; the ones
                // already there are checked and left alone. Each level asks
                // the same question of the same server, so that rule carries
                // down without being restated.
                self.ensure_dir(target, &parent, mode)?;
                match self.mkdir(target, path)? {
                    Made::Created => {
                        self.set_mode(target, path, mode)?;
                        Ok(None)
                    }
                    Made::Refused(reason) => Ok(Some(reason)),
                    // The parent is there now, so the same answer twice means
                    // something is removing directories underneath us. That is
                    // reported rather than chased.
                    Made::ParentMissing => Err(CliftError::new(
                        Stage::Staging,
                        ErrorKind::RemoteDirectory,
                        format!(
                            "{path} still has no parent on {} after Clift created one",
                            target.ssh_host()
                        ),
                    )),
                }
            }
        }
    }

    /// Reads the mode back and holds it to `mode`.
    ///
    /// Creating a directory is not the same as it having the permissions asked
    /// for: a remote umask, or a filesystem that does not carry permissions at
    /// all, would both slip through without this.
    fn confirm_mode(
        &self,
        target: &TransportTarget,
        path: &RemotePath,
        mode: u32,
        refusal: Option<&str>,
    ) -> Result<(), CliftError> {
        match self.stat(target, path)? {
            Some(entry) => check_existing(target, path, mode, &entry),
            // Nothing is there, so "it already exists" was the wrong reading of
            // the refusal. Whatever the server actually said is the answer.
            None => match refusal {
                Some(reason) => Err(sftp_failed(
                    target,
                    &format!("could not create {path}"),
                    reason,
                )),
                None => Err(CliftError::new(
                    Stage::Staging,
                    ErrorKind::RemoteDirectory,
                    format!("{path} was reported as created but is not there"),
                )),
            },
        }
    }

    /// Runs one `mkdir` and reads the outcome off the server's answer.
    fn mkdir(&self, target: &TransportTarget, path: &RemotePath) -> Result<Made, CliftError> {
        let mut batch = SftpBatch::new();
        batch.push("mkdir", &[path.as_str()])?;
        let outcome = self.runner().run_sftp(target, &batch)?;
        if outcome.succeeded() {
            return Ok(Made::Created);
        }
        // The three answers OpenSSH 9.9 gives, collected from a real server:
        //   remote mkdir "...": No such file or directory
        //   remote mkdir "...": Permission denied
        //   remote mkdir "...": Failure
        // The last one is how protocol 3 says "it is already there", and also
        // how it says several other things, so anything unrecognised joins it
        // in the branch that looks rather than assumes.
        match classify(&outcome.stderr) {
            Symptom::RemoteMissing => Ok(Made::ParentMissing),
            Symptom::RemotePermissionDenied => Err(sftp_failed(
                target,
                &format!("could not create {path}"),
                &outcome.stderr,
            )),
            _ => Ok(Made::Refused(outcome.stderr)),
        }
    }

    /// Sets the permissions of a directory Clift has just created.
    fn set_mode(
        &self,
        target: &TransportTarget,
        path: &RemotePath,
        mode: u32,
    ) -> Result<(), CliftError> {
        let mut batch = SftpBatch::new();
        batch.push("chmod", &[&format!("{mode:o}"), path.as_str()])?;
        let outcome = self.runner().run_sftp(target, &batch)?;
        if outcome.succeeded() {
            return Ok(());
        }
        Err(sftp_failed(
            target,
            &format!("could not set the permissions of {path}"),
            &outcome.stderr,
        ))
    }

    /// Metadata for one path, or `None` when it does not exist.
    ///
    /// # Errors
    /// Fails when the parent directory cannot be listed for a reason other
    /// than not existing.
    pub fn stat(
        &self,
        target: &TransportTarget,
        path: &RemotePath,
    ) -> Result<Option<RemoteEntry>, CliftError> {
        let Some(parent) = parent_of(path) else {
            // Metadata is read by listing the parent, and the root has none.
            // Nothing in Clift needs to stat `/`, and returning a made-up entry
            // for it would be worse than saying so.
            return Err(CliftError::new(
                Stage::Staging,
                ErrorKind::RemoteDirectory,
                "the remote root directory cannot be inspected".to_string(),
            ));
        };
        let Some(wanted) = base_name(path) else {
            return Ok(None);
        };

        let Some(entries) = self.list_raw(target, &parent)? else {
            return Ok(None);
        };
        Ok(entries
            .into_iter()
            .find(|(name, _)| name == wanted)
            .map(|(_, entry)| entry))
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
        match self.list_raw(target, path)? {
            Some(entries) => Ok(entries.into_iter().map(|(_, entry)| entry).collect()),
            None => Err(CliftError::new(
                Stage::Staging,
                ErrorKind::RemoteDirectory,
                format!("{path} does not exist on {}", target.ssh_host()),
            )),
        }
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
        let Some(entry) = self.stat(target, path)? else {
            return Ok(());
        };
        let verb = match entry.kind {
            RemoteEntryKind::Directory => "rmdir",
            _ => "rm",
        };

        let mut batch = SftpBatch::new();
        batch.push(verb, &[path.as_str()])?;
        let outcome = self.runner().run_sftp(target, &batch)?;
        if outcome.succeeded() {
            return Ok(());
        }
        Err(sftp_failed(
            target,
            &format!("could not remove {path}"),
            &outcome.stderr,
        ))
    }

    /// Lists a directory, returning `None` when it does not exist.
    ///
    /// Each entry is paired with its raw name so that `stat` can match on it
    /// without going through [`SafeFileName`], which would drop exactly the
    /// entries a caller may need to know about.
    fn list_raw(
        &self,
        target: &TransportTarget,
        path: &RemotePath,
    ) -> Result<Option<Vec<(String, RemoteEntry)>>, CliftError> {
        let mut batch = SftpBatch::new();
        // `-a` is required: Clift's own intermediate uploads are dotfiles.
        batch.push("ls", &["-la", path.as_str()])?;
        let outcome = self.runner().run_sftp(target, &batch)?;
        if !outcome.succeeded() {
            if missing(&outcome.stderr) {
                return Ok(None);
            }
            return Err(sftp_failed(
                target,
                &format!("could not list {path}"),
                &outcome.stderr,
            ));
        }

        let now = SystemTime::now();
        let mut entries = Vec::new();
        for line in outcome.stdout.lines() {
            let Some(parsed) = parse_listing_line(line, now) else {
                continue;
            };
            if parsed.name == "." || parsed.name == ".." {
                continue;
            }
            // A name Clift cannot represent is left out rather than
            // approximated: it is exactly the kind of entry cleanup must not
            // act on.
            let Ok(safe) = SafeFileName::new(parsed.name.clone()) else {
                continue;
            };
            entries.push((
                parsed.name,
                RemoteEntry {
                    name: safe,
                    kind: parsed.kind,
                    size: parsed.size,
                    mode: parsed.mode,
                    modified: parsed.modified,
                },
            ));
        }
        Ok(Some(entries))
    }
}

/// What one `mkdir` says about the path it was given.
enum Made {
    /// It was not there, and now it is, created by this call.
    Created,
    /// The server declined, for a reason that is not a missing ancestor. The
    /// usual one is that it is already there; the server's words are kept
    /// because a full disk is worded identically.
    Refused(String),
    /// A component above it does not exist yet.
    ParentMissing,
}

fn check_existing(
    target: &TransportTarget,
    path: &RemotePath,
    mode: u32,
    existing: &RemoteEntry,
) -> Result<(), CliftError> {
    if existing.kind != RemoteEntryKind::Directory {
        return Err(CliftError::new(
            Stage::Staging,
            ErrorKind::RemoteDirectory,
            format!("{path} on {} is not a directory", target.ssh_host()),
        ));
    }
    match existing.mode {
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

fn sftp_failed(target: &TransportTarget, what: &str, stderr: &str) -> CliftError {
    map_failure(target, Stage::Staging, what, stderr)
}

/// Whether an SFTP failure means "it is not there".
///
/// OpenSSH words this two ways depending on the operation: `ls` reports
/// `Can't ls: "..." not found`, while the file operations report the errno text
/// `No such file or directory`. Matching only the second is how `stat` on a
/// missing directory turned into a transfer error instead of `Ok(None)`.
fn missing(stderr: &str) -> bool {
    classify(stderr) == Symptom::RemoteMissing
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

/// One line of a listing, before it is checked against Clift's own rules.
///
/// Parsing and validation are separated because `.` and `..` are expected in
/// every listing while being invalid [`SafeFileName`]s: folding the two steps
/// together would make a normal entry indistinguishable from a suspicious one.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ParsedLine {
    name: String,
    kind: RemoteEntryKind,
    size: u64,
    mode: Option<u32>,
    modified: Option<SystemTime>,
}

/// Parses one line of `ls -la` as `sftp` renders it.
///
/// The format is `mode links owner group size month day time-or-year name`,
/// and the name may contain spaces, so eight fields are taken from the left and
/// everything after them is the name. When a path argument was given, `sftp`
/// prints each entry as a full path; the directory part is stripped so that
/// callers always see a bare name.
fn parse_listing_line(line: &str, now: SystemTime) -> Option<ParsedLine> {
    let mut rest = line.trim_start();
    let mut fields = Vec::with_capacity(8);
    for _ in 0..8 {
        let end = rest.find(char::is_whitespace)?;
        fields.push(&rest[..end]);
        rest = rest[end..].trim_start();
    }
    if rest.is_empty() {
        return None;
    }

    let permissions = fields[0];
    let kind = match permissions.chars().next()? {
        'd' => RemoteEntryKind::Directory,
        'l' => RemoteEntryKind::Symlink,
        '-' => RemoteEntryKind::File,
        _ => RemoteEntryKind::Other,
    };
    let size = fields[4].parse::<u64>().ok()?;
    let modified = parse_timestamp(fields[5], fields[6], fields[7], now);

    let raw = rest.to_string();
    let name = raw.rsplit('/').next().unwrap_or(&raw).to_string();
    if name.is_empty() {
        return None;
    }

    Some(ParsedLine {
        name,
        kind,
        size,
        mode: parse_mode(permissions),
        modified,
    })
}

/// Turns `drwx------` into `0o700`.
fn parse_mode(permissions: &str) -> Option<u32> {
    let bits: Vec<char> = permissions.chars().skip(1).take(9).collect();
    if bits.len() != 9 {
        return None;
    }
    let mut mode = 0;
    for (index, bit) in bits.iter().enumerate() {
        let value = match index % 3 {
            0 => 0o4,
            1 => 0o2,
            _ => 0o1,
        };
        let set = match (index % 3, bit) {
            (_, '-') => false,
            (0, 'r') | (1, 'w') => true,
            // The execute column doubles as setuid, setgid and sticky.
            (2, 'x' | 's' | 't') => true,
            (2, 'S' | 'T') => false,
            _ => return None,
        };
        if set {
            mode |= value << (6 - 3 * (index / 3));
        }
    }
    Some(mode)
}

/// Parses the three date fields `sftp` prints, which are rendered in UTC
/// because the runner pins the child's `TZ`.
///
/// Two shapes exist: `Aug 30 12:34` for recent entries and `Feb 3 2023` for
/// older ones. The recent form carries no year, so the year is chosen as the
/// most recent one that does not put the entry in the future.
fn parse_timestamp(month: &str, day: &str, last: &str, now: SystemTime) -> Option<SystemTime> {
    let month = month_number(month)?;
    let day: u32 = day.parse().ok()?;
    let now_secs = unix_seconds(now);

    let (year, hour, minute) = match last.split_once(':') {
        Some((hour, minute)) => {
            let (current_year, _, _) = civil_from_days(now_secs.div_euclid(86_400));
            (current_year, hour.parse().ok()?, minute.parse().ok()?)
        }
        None => (last.parse().ok()?, 0, 0),
    };

    let seconds = |year: i64| -> i64 {
        days_from_civil(year, month, day) * 86_400
            + i64::from(hour) * 3_600
            + i64::from(minute) * 60
    };
    let mut candidate = seconds(year);
    if last.contains(':') && candidate > now_secs + 86_400 {
        // A "recent" entry cannot be in the future; it is from last year.
        candidate = seconds(year - 1);
    }
    if candidate < 0 {
        return None;
    }
    Some(UNIX_EPOCH + Duration::from_secs(candidate as u64))
}

fn month_number(name: &str) -> Option<u32> {
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    MONTHS
        .iter()
        .position(|candidate| candidate.eq_ignore_ascii_case(name))
        .map(|index| index as u32 + 1)
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

    fn at(seconds: u64) -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(seconds)
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
    fn permission_columns_become_octal_modes() {
        assert_eq!(parse_mode("drwx------"), Some(0o700));
        assert_eq!(parse_mode("-rw-------"), Some(0o600));
        assert_eq!(parse_mode("drwxr-xr-x"), Some(0o755));
        assert_eq!(parse_mode("-rw-rw-r--"), Some(0o664));
        assert_eq!(parse_mode("drwxrwxrwt"), Some(0o777));
        assert_eq!(parse_mode("short"), None);
    }

    /// The exact bytes OpenSSH 9.9's sftp printed in the test container.
    #[test]
    fn a_listing_line_is_split_into_metadata_and_a_name_that_may_contain_spaces() {
        // 2026-08-31 04:20 UTC.
        let now = at(1_788_150_000);
        let parsed = parse_listing_line(
            "drwx------    ? dev      dev          4096 Aug 30 17:10 .",
            now,
        )
        .unwrap();
        assert_eq!(
            parsed.name, ".",
            "'.' is parsed, then filtered by the caller"
        );
        assert_eq!(parsed.kind, RemoteEntryKind::Directory);
        assert_eq!(parsed.mode, Some(0o700));
        assert_eq!(parsed.size, 4096);

        let parsed = parse_listing_line(
            "-rw-------    ? dev      dev           182 Aug 30 12:34 /home/dev/a b 中文.png",
            now,
        )
        .unwrap();
        assert_eq!(
            parsed.name, "a b 中文.png",
            "the directory part must be stripped and the spaces kept"
        );
        assert_eq!(parsed.kind, RemoteEntryKind::File);
        assert_eq!(parsed.mode, Some(0o600));
        assert_eq!(parsed.size, 182);
        assert_eq!(
            parsed
                .modified
                .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
                .map(|since| since.as_secs()),
            Some(1_788_093_240),
            "2026-08-30 12:34 UTC"
        );
    }

    #[test]
    fn timestamps_are_read_back_as_the_utc_instant_sftp_rendered() {
        let now = at(1_788_150_000);
        let parsed = parse_timestamp("Aug", "30", "12:34", now).unwrap();
        assert_eq!(
            parsed.duration_since(UNIX_EPOCH).unwrap().as_secs(),
            1_788_093_240
        );

        let old = parse_timestamp("Feb", "3", "2023", now).unwrap();
        assert_eq!(
            old.duration_since(UNIX_EPOCH).unwrap().as_secs(),
            1_675_382_400
        );
    }

    #[test]
    fn a_recent_timestamp_that_would_land_in_the_future_belongs_to_last_year() {
        // "now" is 2026-01-05; an entry stamped 30 December is from 2025.
        let now = at(1_767_571_200);
        let parsed = parse_timestamp("Dec", "30", "10:00", now).unwrap();
        let (year, month, day) =
            civil_from_days(parsed.duration_since(UNIX_EPOCH).unwrap().as_secs() as i64 / 86_400);
        assert_eq!((year, month, day), (2025, 12, 30));
    }
}
