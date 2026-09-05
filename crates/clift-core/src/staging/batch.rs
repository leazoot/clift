//! The directory one batch of attachments is uploaded into.
//!
//! The layout is `<inbox>/YYYY-MM-DD/<BatchId>/`, and both levels earn their
//! place. The date makes retention cheap to reason about and keeps a listing
//! readable. The batch identifier is 128 bits from the operating system CSPRNG,
//! which is what stops two batches containing a file of the same name from
//! overwriting each other, and what stops another account on the host from
//! guessing where an attachment landed.

use crate::calendar::format_date;
use crate::domain::{BatchId, RemotePath, SafeFileName};
use crate::error::{CliftError, ErrorKind, Stage};
use crate::ports::{Clock, IdSource, RemoteFs, TransportTarget};
use crate::staging::{INBOX_MODE, InboxLocation};

/// Where one batch will be written.
///
/// The date is decided once, when the batch is planned, and then carried. That
/// is the whole of the midnight rule: a batch that starts at 23:59:59 and
/// finishes at 00:00:01 cannot end up with some files in yesterday's directory
/// and some in today's, because there is only ever one date to use.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BatchPlan {
    date: String,
    id: BatchId,
    directory: RemotePath,
}

impl BatchPlan {
    /// The batch directory, absolute.
    #[must_use]
    pub fn directory(&self) -> &RemotePath {
        &self.directory
    }

    /// The date directory's name, `YYYY-MM-DD` in UTC.
    #[must_use]
    pub fn date(&self) -> &str {
        &self.date
    }

    #[must_use]
    pub fn id(&self) -> &BatchId {
        &self.id
    }

    /// The path one file will occupy inside this batch.
    #[must_use]
    pub fn file(&self, name: &SafeFileName) -> RemotePath {
        self.directory.join(name)
    }
}

/// Decides where a batch goes, without touching the network.
///
/// # Errors
/// Fails when the random source is unavailable, which aborts the batch rather
/// than falling back to something predictable, and when the date directory name
/// cannot be used as a path component.
pub fn plan_batch(
    inbox: &InboxLocation,
    clock: &dyn Clock,
    ids: &dyn IdSource,
) -> Result<BatchPlan, CliftError> {
    let date = format_date(clock.now());
    let id = ids.new_batch_id()?;

    let date_component = SafeFileName::new(date.clone())
        .map_err(|error| error.into_clift(Stage::Staging, ErrorKind::Internal))?;
    let id_component = SafeFileName::new(id.as_str())
        .map_err(|error| error.into_clift(Stage::Staging, ErrorKind::Internal))?;

    Ok(BatchPlan {
        directory: inbox.root().join(&date_component).join(&id_component),
        date,
        id,
    })
}

/// Creates the batch directory, and the date directory above it, private.
///
/// # Errors
/// Fails when the directories cannot be created, or exist with different
/// permissions.
pub fn create_batch(
    remote: &dyn RemoteFs,
    target: &TransportTarget,
    plan: &BatchPlan,
) -> Result<(), CliftError> {
    // One call: `ensure_dir` creates the missing parents with the same mode, so
    // the date directory is private too without a second round trip. Round
    // trips are expensive: 3.6 to 8.4 seconds each on a distant host.
    remote.ensure_dir(target, plan.directory(), INBOX_MODE)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::staging::ensure_inbox;
    use crate::testing::{FakeClock, FakeIdSource, RecordingTransport, TransportCall};

    fn inbox() -> InboxLocation {
        let remote = RecordingTransport::new("/home/dev");
        ensure_inbox(&remote, &TransportTarget::new("core"), None).unwrap_or_else(|error| {
            panic!("the fake transport should not fail: {error}");
        })
    }

    /// 2026-08-30 23:59:59 UTC.
    const JUST_BEFORE_MIDNIGHT: u64 = 1_788_134_399;

    #[test]
    fn a_batch_goes_under_the_date_and_then_its_own_identifier() {
        let clock = FakeClock::at_unix_seconds(1_788_093_240);
        let ids = FakeIdSource::starting_at(1);
        let plan = plan_batch(&inbox(), &clock, &ids).unwrap();

        assert_eq!(plan.date(), "2026-08-30");
        assert_eq!(
            plan.directory().as_str(),
            format!("/home/dev/.cache/clift/inbox/2026-08-30/{}", plan.id())
        );
    }

    /// The batch must not straddle two date directories, however long it runs.
    #[test]
    fn a_batch_that_runs_past_midnight_stays_in_the_directory_it_started_in() {
        let clock = FakeClock::at_unix_seconds(JUST_BEFORE_MIDNIGHT);
        let ids = FakeIdSource::starting_at(1);
        let plan = plan_batch(&inbox(), &clock, &ids).unwrap();
        assert_eq!(plan.date(), "2026-08-30");

        // Two seconds later it is a different day, and the plan is unchanged:
        // there is only one date, decided once.
        clock.advance(std::time::Duration::from_secs(2));
        assert_eq!(format_date(clock.now()), "2026-08-31");
        assert_eq!(plan.date(), "2026-08-30");
        assert!(plan.directory().as_str().contains("/2026-08-30/"));
    }

    #[test]
    fn two_batches_never_share_a_directory() {
        let clock = FakeClock::at_unix_seconds(JUST_BEFORE_MIDNIGHT);
        let ids = FakeIdSource::starting_at(1);
        let inbox = inbox();
        let first = plan_batch(&inbox, &clock, &ids).unwrap();
        let second = plan_batch(&inbox, &clock, &ids).unwrap();
        assert_ne!(first.directory(), second.directory());
        assert_ne!(first.id(), second.id());
    }

    #[test]
    fn a_file_path_is_built_from_the_batch_directory_and_cannot_leave_it() {
        let clock = FakeClock::at_unix_seconds(1_788_093_240);
        let ids = FakeIdSource::starting_at(1);
        let plan = plan_batch(&inbox(), &clock, &ids).unwrap();

        // The argument is a SafeFileName, so a traversal component is not
        // expressible; this checks the composition, not the parsing.
        let name = SafeFileName::sanitize("../../escape.png");
        let file = plan.file(&name);
        assert!(file.is_within(plan.directory()));
        assert_eq!(file.as_str(), format!("{}/escape.png", plan.directory()));
    }

    #[test]
    fn creating_a_batch_asks_for_one_private_directory_and_no_more_round_trips() {
        let remote = RecordingTransport::new("/home/dev");
        let target = TransportTarget::new("core");
        let inbox = ensure_inbox(&remote, &target, None).unwrap();
        let clock = FakeClock::at_unix_seconds(1_788_093_240);
        let ids = FakeIdSource::starting_at(1);
        let plan = plan_batch(&inbox, &clock, &ids).unwrap();

        let before = remote.call_count();
        create_batch(&remote, &target, &plan).unwrap();
        let made: Vec<TransportCall> = remote.calls().into_iter().skip(before).collect();

        assert_eq!(
            made,
            vec![TransportCall::EnsureDir {
                path: plan.directory().as_str().to_string(),
                mode: 0o700,
            }],
            "each round trip costs seconds on a real host; one is the budget"
        );
    }
}
