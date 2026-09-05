//! Uploading a batch so that it either lands whole or leaves nothing behind.
//!
//! Per-file atomicity is the transport's job (upload to a temporary name,
//! verify the size, rename). This module owns the batch-level half of the all-or-nothing rule:
//! if any file in a batch fails, the caller gets no remote path at all, not
//! even for the files that did arrive. Handing over half a batch would put
//! paths in front of an agent for attachments the user believes were sent
//! together, and the agent has no way to tell that the set is short.

use crate::domain::{BatchNames, LocalAttachment, RemotePath, SafeFileName};
use crate::error::CliftError;
use crate::ports::{RemoteFs, RemoteUpload, TransportTarget};
use crate::staging::BatchPlan;

/// One attachment, where it landed and how big it turned out to be.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StagedFile {
    name: SafeFileName,
    path: RemotePath,
    size: u64,
}

impl StagedFile {
    #[must_use]
    pub fn name(&self) -> &SafeFileName {
        &self.name
    }

    /// The absolute remote path, which is what an agent is given.
    #[must_use]
    pub fn path(&self) -> &RemotePath {
        &self.path
    }

    /// The size the remote host reported after the upload.
    #[must_use]
    pub const fn size(&self) -> u64 {
        self.size
    }
}

/// A batch that arrived complete.
///
/// This type only exists on the success path. There is no partially staged
/// variant, so a caller cannot accidentally render paths from a batch that
/// failed halfway.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StagedBatch {
    directory: RemotePath,
    files: Vec<StagedFile>,
}

impl StagedBatch {
    #[must_use]
    pub fn directory(&self) -> &RemotePath {
        &self.directory
    }

    #[must_use]
    pub fn files(&self) -> &[StagedFile] {
        &self.files
    }
}

/// Uploads every attachment into an already created batch directory.
///
/// Names are assigned through [`BatchNames`], so two attachments that sanitise
/// to the same name are given distinct ones instead of one overwriting the
/// other.
///
/// # Errors
/// Fails as soon as one upload fails, and returns that failure unchanged. What
/// had already been uploaded is removed on a best-effort basis first; a cleanup
/// failure is swallowed, because replacing "the transfer failed" with "the
/// tidying up failed" would hide the problem the user has to fix.
pub fn stage_batch<T>(
    transport: &T,
    target: &TransportTarget,
    plan: &BatchPlan,
    attachments: &[LocalAttachment],
) -> Result<StagedBatch, CliftError>
where
    T: RemoteFs + RemoteUpload + ?Sized,
{
    let mut names = BatchNames::new();
    let mut staged: Vec<StagedFile> = Vec::with_capacity(attachments.len());

    for attachment in attachments {
        let name = names.assign(attachment.name().as_str());
        let path = plan.file(&name);

        match transport.upload_atomic(target, attachment.path(), &path) {
            Ok(size) => staged.push(StagedFile { name, path, size }),
            Err(error) => {
                discard(transport, target, &staged, plan);
                return Err(error);
            }
        }
    }

    Ok(StagedBatch {
        directory: plan.directory().clone(),
        files: staged,
    })
}

/// Removes what a failed batch already put on the host, ignoring failures.
///
/// The batch directory goes too: leaving an empty private directory behind on
/// every failed send would accumulate silently, and it is empty only because
/// everything inside it was just removed.
fn discard<T>(transport: &T, target: &TransportTarget, staged: &[StagedFile], plan: &BatchPlan)
where
    T: RemoteFs + ?Sized,
{
    for file in staged {
        let _ = transport.remove(target, file.path());
    }
    let _ = transport.remove(target, plan.directory());
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::FileKind;
    use crate::ports::Clock;
    use crate::staging::{ensure_inbox, plan_batch};
    use crate::testing::{FakeClock, FakeIdSource, RecordingTransport, TransportCall};
    use std::path::PathBuf;

    fn attachment(local: &str, name: &str) -> LocalAttachment {
        LocalAttachment::new(
            PathBuf::from(local),
            SafeFileName::sanitize(name),
            7,
            FileKind::Regular,
        )
        .unwrap_or_else(|error| panic!("bad test attachment: {error}"))
    }

    struct Fixture {
        transport: RecordingTransport,
        target: TransportTarget,
        plan: BatchPlan,
    }

    fn fixture() -> Fixture {
        let transport = RecordingTransport::new("/home/dev");
        let target = TransportTarget::new("core");
        let inbox = ensure_inbox(&transport, &target, None).unwrap();
        let clock = FakeClock::at_unix_seconds(1_788_093_240);
        let ids = FakeIdSource::starting_at(1);
        let plan = plan_batch(&inbox, &clock, &ids).unwrap();
        let _ = clock.now();
        Fixture {
            transport,
            target,
            plan,
        }
    }

    #[test]
    fn every_file_lands_inside_the_batch_directory() {
        let fixture = fixture();
        let batch = stage_batch(
            &fixture.transport,
            &fixture.target,
            &fixture.plan,
            &[
                attachment("/tmp/a.png", "shot.png"),
                attachment("/tmp/b.txt", "notes.txt"),
            ],
        )
        .unwrap();

        assert_eq!(batch.files().len(), 2);
        for file in batch.files() {
            assert!(
                file.path().is_within(fixture.plan.directory()),
                "{} escaped the batch directory",
                file.path()
            );
        }
    }

    #[test]
    fn two_attachments_with_the_same_name_do_not_overwrite_each_other() {
        let fixture = fixture();
        let batch = stage_batch(
            &fixture.transport,
            &fixture.target,
            &fixture.plan,
            &[
                attachment("/tmp/one/shot.png", "shot.png"),
                attachment("/tmp/two/shot.png", "shot.png"),
            ],
        )
        .unwrap();

        let first = batch.files()[0].path().as_str();
        let second = batch.files()[1].path().as_str();
        assert_ne!(first, second);
        assert!(second.ends_with("/shot-2.png"), "{second}");
    }

    /// One failure means no paths, not a shorter list.
    #[test]
    fn a_batch_that_fails_halfway_yields_no_paths_at_all() {
        let fixture = fixture();
        let doomed = fixture
            .plan
            .file(&SafeFileName::sanitize("second.txt"))
            .as_str()
            .to_string();
        fixture.transport.fail_upload_of(&doomed);

        let error = stage_batch(
            &fixture.transport,
            &fixture.target,
            &fixture.plan,
            &[
                attachment("/tmp/a.png", "first.png"),
                attachment("/tmp/b.txt", "second.txt"),
            ],
        )
        .expect_err("the second upload was made to fail");

        // There is no partially staged value to inspect: the type has no such
        // variant, so "some paths came back" is not expressible. What is worth
        // asserting is that the transport's failure reaches the caller
        // unchanged -- the use case decides flow, never error semantics.
        assert_eq!(error.exit_code().as_u8(), 23);
        assert_eq!(error.stage(), crate::error::Stage::Transfer);
        assert!(
            error.to_string().contains("injected upload failure"),
            "the original failure must not be rewritten: {error}"
        );
    }

    #[test]
    fn what_a_failed_batch_already_uploaded_is_removed_again() {
        let fixture = fixture();
        let first = fixture.plan.file(&SafeFileName::sanitize("first.png"));
        let doomed = fixture.plan.file(&SafeFileName::sanitize("second.txt"));
        fixture.transport.fail_upload_of(doomed.as_str());

        let before = fixture.transport.call_count();
        let _ = stage_batch(
            &fixture.transport,
            &fixture.target,
            &fixture.plan,
            &[
                attachment("/tmp/a.png", "first.png"),
                attachment("/tmp/b.txt", "second.txt"),
            ],
        );
        let made: Vec<TransportCall> = fixture
            .transport
            .calls()
            .into_iter()
            .skip(before)
            .filter(|call| matches!(call, TransportCall::Remove { .. }))
            .collect();

        assert_eq!(
            made,
            vec![
                TransportCall::Remove {
                    path: first.as_str().to_string(),
                },
                TransportCall::Remove {
                    path: fixture.plan.directory().as_str().to_string(),
                },
            ],
            "the file that did arrive, and then the directory it was alone in"
        );
    }

    #[test]
    fn nothing_is_removed_when_the_first_file_is_the_one_that_fails() {
        let fixture = fixture();
        let doomed = fixture.plan.file(&SafeFileName::sanitize("only.png"));
        fixture.transport.fail_upload_of(doomed.as_str());

        let before = fixture.transport.call_count();
        let _ = stage_batch(
            &fixture.transport,
            &fixture.target,
            &fixture.plan,
            &[attachment("/tmp/a.png", "only.png")],
        );
        let removals: Vec<TransportCall> = fixture
            .transport
            .calls()
            .into_iter()
            .skip(before)
            .filter(|call| matches!(call, TransportCall::Remove { .. }))
            .collect();

        assert_eq!(
            removals,
            vec![TransportCall::Remove {
                path: fixture.plan.directory().as_str().to_string(),
            }],
            "only the empty batch directory is left to remove"
        );
    }

    #[test]
    fn an_empty_batch_uploads_nothing() {
        let fixture = fixture();
        let before = fixture.transport.call_count();
        let batch = stage_batch(&fixture.transport, &fixture.target, &fixture.plan, &[]).unwrap();
        assert!(batch.files().is_empty());
        assert_eq!(fixture.transport.call_count(), before);
    }
}
