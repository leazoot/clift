//! Removing old batches, and refusing to remove anything else.
//!
//! This is the most dangerous code in Clift. Everything else either adds a file
//! or fails; this deletes, and a deletion that goes one directory too far is
//! not a bug report, it is somebody's data.
//!
//! So the rule is written as four steps, and each is a refusal:
//!
//! 1. resolve the inbox root and treat it as the only place deletion may occur;
//! 2. when listing, do not follow a symbolic link that leaves it;
//! 3. before each deletion, check **again** that the path is still inside it;
//! 4. if anything is unclear, skip it and say so. Never delete on a guess.
//!
//! Step 3 looks redundant after step 1, and is not: the paths being deleted
//! come from a listing the remote host produced, and the host is not something
//! Clift controls.

use crate::domain::RemotePath;
use crate::error::CliftError;
use crate::ports::{RemoteEntry, RemoteEntryKind, RemoteFs, TransportTarget};
use std::time::{Duration, SystemTime};

/// Which batches a run of cleanup should remove.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Retention {
    /// Everything older than this.
    OlderThan(Duration),
    /// Everything, whatever its age.
    Everything,
}

/// Whether a run removes anything, or only says what it would remove.
///
/// `Report` walks exactly the same path as `Remove` and applies exactly the
/// same refusals; the only difference is that it does not delete. A dry run
/// that took a different route would not be telling the truth about the real
/// one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Remove,
    Report,
}

/// What one cleanup run did, and what it deliberately did not do.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CleanReport {
    /// Batch directories removed.
    pub batches: usize,
    /// Files removed inside them.
    pub files: usize,
    /// Bytes those files occupied, as the host reported them.
    pub bytes: u64,
    /// Everything skipped, and why. Never empty for a reason Clift invented:
    /// each entry is something the user may want to look at by hand.
    pub skipped: Vec<String>,
}

impl CleanReport {
    fn skip(&mut self, path: &RemotePath, reason: &str) {
        self.skipped.push(format!("{path}: {reason}"));
    }
}

/// Removes expired batches from an inbox.
///
/// `now` is passed in rather than read, so that "older than 24 hours" can be
/// tested without waiting a day.
///
/// # Errors
/// Fails only when the inbox itself cannot be listed. A single directory that
/// cannot be read is recorded as skipped and the rest of the run continues:
/// one unreadable batch must not stop the others from being cleaned up.
pub fn clean(
    remote: &dyn RemoteFs,
    target: &TransportTarget,
    inbox: &RemotePath,
    retention: Retention,
    action: Action,
    now: SystemTime,
) -> Result<CleanReport, CliftError> {
    let mut report = CleanReport::default();

    // Step 1. Nothing below is allowed outside this.
    let root = inbox.clone();

    // The one failure that stops the run: if the inbox itself cannot be listed
    // there is nothing to reason about, and guessing is what this module is for
    // avoiding.
    let dates = remote.list_dir(target, &root)?;

    for date in dates {
        let path = root.join(&date.name);
        if !within(&path, &root) {
            report.skip(&path, "outside the inbox");
            continue;
        }
        // Step 2. A link is unlinked or left alone, never walked through: the
        // thing on the other side is not Clift's to enumerate.
        if date.kind != RemoteEntryKind::Directory {
            report.skip(&path, "not a date directory");
            continue;
        }

        let batches = match remote.list_dir(target, &path) {
            Ok(entries) => entries,
            Err(error) => {
                report.skip(&path, &format!("could not be listed: {}", error.message()));
                continue;
            }
        };

        let mut emptied = 0usize;
        for batch in &batches {
            let batch_path = path.join(&batch.name);
            match remove_batch(
                remote,
                target,
                &root,
                &batch_path,
                batch,
                retention,
                action,
                now,
            ) {
                Removal::Removed { files, bytes } => {
                    report.batches += 1;
                    report.files += files;
                    report.bytes += bytes;
                    emptied += 1;
                }
                Removal::Kept => {}
                Removal::Skipped(reason) => report.skip(&batch_path, &reason),
            }
        }

        // A date directory is removed only once everything in it has gone, and
        // only if it was Clift that emptied it.
        if emptied == batches.len()
            && !batches.is_empty()
            && within(&path, &root)
            && action == Action::Remove
        {
            let _ = remote.remove(target, &path);
        }
    }

    Ok(report)
}

enum Removal {
    Removed { files: usize, bytes: u64 },
    Kept,
    Skipped(String),
}

#[expect(
    clippy::too_many_arguments,
    reason = "each argument is a distinct fact this decision needs; bundling \
              them would hide which ones the refusals actually depend on"
)]
fn remove_batch(
    remote: &dyn RemoteFs,
    target: &TransportTarget,
    root: &RemotePath,
    path: &RemotePath,
    entry: &RemoteEntry,
    retention: Retention,
    action: Action,
    now: SystemTime,
) -> Removal {
    if !within(path, root) {
        return Removal::Skipped("outside the inbox".to_string());
    }
    if entry.kind == RemoteEntryKind::Symlink {
        // Not followed, and not removed either: a link Clift did not create is
        // not Clift's to delete, and the thing it points at certainly is not.
        return Removal::Skipped("a symbolic link, which Clift does not follow".to_string());
    }
    if entry.kind != RemoteEntryKind::Directory {
        return Removal::Skipped("not a batch directory".to_string());
    }

    if let Retention::OlderThan(age) = retention {
        let Some(modified) = entry.modified else {
            // No timestamp, no decision. Deleting something whose age is
            // unknown is exactly the guess this module exists to avoid.
            return Removal::Skipped("the host reported no modification time".to_string());
        };
        match now.duration_since(modified) {
            Ok(elapsed) if elapsed >= age => {}
            // Either it is young enough, or its timestamp is in the future --
            // a clock difference, which is not a reason to delete anything.
            _ => return Removal::Kept,
        }
    }

    let files = match remote.list_dir(target, path) {
        Ok(entries) => entries,
        Err(error) => {
            return Removal::Skipped(format!("could not be listed: {}", error.message()));
        }
    };

    let mut removed = 0usize;
    let mut bytes = 0u64;
    for file in &files {
        let file_path = path.join(&file.name);
        // Step 3. Checked again, against a path the *host* produced.
        if !within(&file_path, root) {
            return Removal::Skipped(format!("{file_path} is outside the inbox"));
        }
        if file.kind == RemoteEntryKind::Directory {
            return Removal::Skipped(
                "contains a directory, which Clift never creates in a batch".to_string(),
            );
        }
        if action == Action::Remove && remote.remove(target, &file_path).is_err() {
            return Removal::Skipped(format!("{file_path} could not be removed"));
        }
        removed += 1;
        bytes += file.size;
    }

    if action == Action::Remove && remote.remove(target, path).is_err() {
        return Removal::Skipped("the directory itself could not be removed".to_string());
    }
    Removal::Removed {
        files: removed,
        bytes,
    }
}

/// Whether a path is inside the inbox, with the inbox itself not counting.
///
/// `RemotePath::is_within` does the prefix work, including the part people get
/// wrong: `/home/dev/inbox-other` is not inside `/home/dev/inbox`.
fn within(path: &RemotePath, root: &RemotePath) -> bool {
    path != root && path.is_within(root)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::SafeFileName;

    fn path(value: &str) -> RemotePath {
        RemotePath::new(value).unwrap_or_else(|error| panic!("bad test path: {error}"))
    }

    #[test]
    fn only_paths_strictly_inside_the_inbox_are_ever_candidates() {
        let root = path("/home/dev/.cache/clift/inbox");

        assert!(within(
            &path("/home/dev/.cache/clift/inbox/2026-08-30"),
            &root
        ));
        assert!(within(
            &path("/home/dev/.cache/clift/inbox/2026-08-30/ab/shot.png"),
            &root
        ));

        // The root itself is not a candidate: cleanup empties the inbox, it
        // does not remove it.
        assert!(!within(&root, &root));
        // A sibling whose name merely starts the same way.
        assert!(!within(&path("/home/dev/.cache/clift/inbox-old"), &root));
        assert!(!within(&path("/home/dev/.cache/clift"), &root));
        assert!(!within(&path("/etc/passwd"), &root));
        assert!(!within(&path("/"), &root));
    }

    #[test]
    fn a_skipped_entry_says_which_path_and_why() {
        let mut report = CleanReport::default();
        report.skip(&path("/home/dev/inbox/x"), "a symbolic link");
        assert_eq!(report.skipped, ["/home/dev/inbox/x: a symbolic link"]);
    }

    fn entry(name: &str, kind: RemoteEntryKind, modified: Option<SystemTime>) -> RemoteEntry {
        RemoteEntry {
            name: SafeFileName::sanitize(name),
            kind,
            size: 10,
            mode: Some(0o700),
            modified,
        }
    }

    /// A batch whose age cannot be established is left alone.
    #[test]
    fn a_batch_with_no_timestamp_is_skipped_rather_than_removed() {
        let root = path("/home/dev/inbox");
        let batch = path("/home/dev/inbox/2026-08-30/ab");
        let outcome = remove_batch(
            &crate::testing::RecordingTransport::new("/home/dev"),
            &TransportTarget::new("core"),
            &root,
            &batch,
            &entry("ab", RemoteEntryKind::Directory, None),
            Retention::OlderThan(Duration::from_secs(60)),
            Action::Remove,
            SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000),
        );
        match outcome {
            Removal::Skipped(reason) => {
                assert!(reason.contains("no modification time"), "{reason}")
            }
            _ => panic!("a batch of unknown age must not be removed"),
        }
    }

    /// A symbolic link inside the inbox is neither followed nor deleted.
    #[test]
    fn a_symbolic_link_is_left_exactly_where_it_is() {
        let root = path("/home/dev/inbox");
        let link = path("/home/dev/inbox/2026-08-30/etcetera");
        let transport = crate::testing::RecordingTransport::new("/home/dev");
        let outcome = remove_batch(
            &transport,
            &TransportTarget::new("core"),
            &root,
            &link,
            &entry(
                "etcetera",
                RemoteEntryKind::Symlink,
                Some(SystemTime::UNIX_EPOCH),
            ),
            Retention::Everything,
            Action::Remove,
            SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000),
        );
        match outcome {
            Removal::Skipped(reason) => assert!(reason.contains("symbolic link"), "{reason}"),
            _ => panic!("a link must not be removed"),
        }
        assert_eq!(
            transport.call_count(),
            0,
            "nothing at all should have been asked of the host"
        );
    }

    /// A future timestamp is a clock difference, not a reason to delete.
    #[test]
    fn a_batch_from_the_future_is_kept() {
        let root = path("/home/dev/inbox");
        let batch = path("/home/dev/inbox/2026-08-30/ab");
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000);
        let outcome = remove_batch(
            &crate::testing::RecordingTransport::new("/home/dev"),
            &TransportTarget::new("core"),
            &root,
            &batch,
            &entry(
                "ab",
                RemoteEntryKind::Directory,
                Some(now + Duration::from_secs(10_000)),
            ),
            Retention::OlderThan(Duration::from_secs(60)),
            Action::Remove,
            now,
        );
        assert!(matches!(outcome, Removal::Kept));
    }
}
