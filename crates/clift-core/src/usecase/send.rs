//! Putting one batch of attachments on a remote host.
//!
//! The ordering here is the requirement, not an implementation detail. The
//! limit check runs before the batch
//! directory is created and before the first byte is sent, so an oversized
//! batch costs one local comparison and leaves nothing at all on the host --
//! no directory, no `.part` file.

use crate::domain::{Limits, LocalAttachment, RemotePath};
use crate::error::{CliftError, ErrorKind, Remedy, Stage};
use crate::format::render;
use crate::ports::{Clock, IdSource, RemoteFs, RemoteUpload, TransportTarget};
use crate::staging::{
    Action, BatchPlan, InboxLocation, Retention, StagedBatch, clean, create_batch,
    ensure_inbox_from, plan_batch, stage_batch,
};
use std::time::Duration;

/// Validates a batch against the limits in force and, if it passes, uploads it.
///
/// # Errors
/// Returns exit code 26 when a limit is exceeded, naming the ceiling that was
/// crossed. Otherwise fails with whatever the transport failed with; a partial
/// batch is never returned, because [`StagedBatch`] has no such state.
pub fn stage_attachments<T>(
    transport: &T,
    target: &TransportTarget,
    plan: &BatchPlan,
    limits: Limits,
    attachments: &[LocalAttachment],
) -> Result<StagedBatch, CliftError>
where
    T: RemoteFs + RemoteUpload,
{
    limits.check(attachments).map_err(|error| {
        error
            .into_clift(Stage::Staging, ErrorKind::LimitExceeded)
            .with_remedy(Remedy::new(
                "Send fewer or smaller files, or raise the limit:",
                format!("clift config set {} <value>", limits.key_for(attachments)),
            ))
    })?;

    create_batch(transport, target, plan)?;
    stage_batch(transport, target, plan, attachments)
}

/// Everything one successful send produced.
///
/// Only exists on the success path, like [`StagedBatch`]: there is no value
/// here that a caller could render after a partial failure, because there is no
/// such value at all.
#[derive(Debug, Clone)]
pub struct SendOutcome {
    batch: StagedBatch,
    insertion_text: String,
    inbox_warning: Option<String>,
    batch_id: String,
    inbox_root: RemotePath,
}

impl SendOutcome {
    #[must_use]
    pub const fn batch(&self) -> &StagedBatch {
        &self.batch
    }

    /// The text the user pastes. Never produced unless every file arrived.
    #[must_use]
    pub fn insertion_text(&self) -> &str {
        &self.insertion_text
    }

    /// What the inbox had to say for itself, if anything -- for instance that
    /// the host nominated a cache directory Clift declined to use.
    #[must_use]
    pub fn inbox_warning(&self) -> Option<&str> {
        self.inbox_warning.as_deref()
    }
}

/// What the configuration says about one send.
///
/// Grouped because they arrive together, from the same file, and because a
/// function with each of them spelled out separately reads as a list of
/// unrelated knobs rather than as "the settings in force".
#[derive(Debug, Clone, Copy, Default)]
pub struct SendPolicy<'a> {
    pub limits: Limits,
    /// Where the user asked for the inbox to be, if they asked.
    pub remote_dir: Option<&'a str>,
    /// The remote home `setup` recorded, if it recorded one. With it the send
    /// does not ask the host again.
    pub remote_home: Option<&'a RemotePath>,
    /// The cache directory `setup` recorded, if it recorded one. With it the
    /// send does not ask the host again.
    pub remote_cache_home: Option<&'a RemotePath>,
}

/// Sends one batch of attachments, from limits to insertion text.
///
/// The order is the requirement, and the parts
/// of it that are load-bearing are:
///
/// - **limits before anything reaches the host**, so an oversized batch costs
///   nothing;
/// - **the insertion text only after every upload succeeded**, which is the all-or-nothing rule
///   and the reason this function returns one value rather than filling in a
///   result as it goes.
///
/// Deciding *what* to send and *where* happens before this: an ordinary text
/// paste must not open a connection, so this function is never reached for one.
///
/// # Errors
/// Fails at the first step that fails, with that step's error unchanged.
pub fn perform<T>(
    transport: &T,
    target: &TransportTarget,
    attachments: &[LocalAttachment],
    policy: &SendPolicy<'_>,
    clock: &dyn Clock,
    ids: &dyn IdSource,
) -> Result<SendOutcome, CliftError>
where
    T: RemoteFs + RemoteUpload,
{
    let SendPolicy {
        limits,
        remote_dir,
        remote_home,
        remote_cache_home,
    } = *policy;
    // Before the first round trip, not merely before the first byte: an
    // oversized batch should not even open a connection.
    limits.check(attachments).map_err(|error| {
        error
            .into_clift(Stage::Staging, ErrorKind::LimitExceeded)
            .with_remedy(Remedy::new(
                "Send fewer or smaller files, or raise the limit:",
                format!("clift config set {} <value>", limits.key_for(attachments)),
            ))
    })?;

    let inbox: InboxLocation = ensure_inbox_from(
        transport,
        target,
        remote_home,
        remote_cache_home,
        remote_dir,
    )?;
    let plan = plan_batch(&inbox, clock, ids)?;
    let batch = stage_attachments(transport, target, &plan, limits, attachments)?;

    let paths: Vec<RemotePath> = batch
        .files()
        .iter()
        .map(|file| file.path().clone())
        .collect();

    Ok(SendOutcome {
        insertion_text: render(&paths),
        inbox_warning: inbox.warning(),
        batch_id: plan.id().as_str().to_string(),
        inbox_root: inbox.root().clone(),
        batch,
    })
}

/// The occasional tidy-up of expired batches, for after a send has been
/// delivered.
///
/// Not part of [`perform`] so that it cannot stand between a key press and the
/// text it produces: a sweep lists directories, and on a distant host that is
/// seconds the user would spend looking at an empty prompt. Whatever it finds
/// or fails to do is a note, never a change to the send it follows.
#[must_use]
pub fn tidy_after<T>(
    outcome: &SendOutcome,
    transport: &T,
    target: &TransportTarget,
    retention: Option<Duration>,
    clock: &dyn Clock,
) -> Option<String>
where
    T: RemoteFs,
{
    sweep_after(
        &outcome.batch_id,
        retention,
        transport,
        target,
        &outcome.inbox_root,
        clock,
    )
}

/// One send in [`SWEEP_ONE_IN`] also tidies up expired batches.
///
/// The specification asks for cleanup that is occasional and best-effort. Occasional
/// because a full listing on every send would add a round trip to the one
/// operation whose latency the user actually feels; best-effort because the
/// attachments have already arrived, and a failure to tidy up is not a reason
/// to tell the user their send failed.
///
/// The die is the batch identifier's own first byte -- already drawn from the
/// operating system CSPRNG, so no clock, no counter and no persisted state.
pub const SWEEP_ONE_IN: u8 = 8;

fn should_sweep(batch_id: &str) -> bool {
    let Some(first) = batch_id
        .get(0..2)
        .and_then(|pair| u8::from_str_radix(pair, 16).ok())
    else {
        // An identifier this function cannot read is not a reason to start
        // deleting things.
        return false;
    };
    first < u8::MAX / SWEEP_ONE_IN
}

/// Runs the occasional sweep, swallowing everything it might have to say.
fn sweep_after<T>(
    batch_id: &str,
    retention: Option<Duration>,
    transport: &T,
    target: &TransportTarget,
    inbox: &RemotePath,
    clock: &dyn Clock,
) -> Option<String>
where
    T: RemoteFs,
{
    let retention = retention?;
    if !should_sweep(batch_id) {
        return None;
    }
    match clean(
        transport,
        target,
        inbox,
        Retention::OlderThan(retention),
        Action::Remove,
        clock.now(),
    ) {
        Ok(report) if report.batches > 0 => Some(format!(
            "also removed {} expired batch(es) from {}",
            report.batches,
            target.ssh_host()
        )),
        Ok(_) => None,
        // Swallowed on purpose. The attachments are already there; a failed
        // tidy-up is a note, not an outcome.
        Err(error) => Some(format!(
            "could not tidy up old batches: {}",
            error.message()
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{FileKind, SafeFileName};
    use crate::staging::{ensure_inbox, plan_batch};
    use crate::testing::{FakeClock, FakeIdSource, RecordingTransport};
    use std::path::PathBuf;

    fn attachment(name: &str, size: u64) -> LocalAttachment {
        LocalAttachment::new(
            PathBuf::from(format!("/tmp/{name}")),
            SafeFileName::sanitize(name),
            size,
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
        let plan = plan_batch(
            &inbox,
            &FakeClock::at_unix_seconds(1_788_093_240),
            &FakeIdSource::starting_at(1),
        )
        .unwrap();
        Fixture {
            transport,
            target,
            plan,
        }
    }

    /// The limit check, stated as a call count: an oversized batch must not reach the
    /// host at all. Not one directory, not one byte.
    #[test]
    fn an_oversized_batch_touches_the_host_not_at_all() {
        let fixture = fixture();
        let too_big = [attachment("huge.mov", 51 * 1024 * 1024)];

        let before = fixture.transport.call_count();
        let error = stage_attachments(
            &fixture.transport,
            &fixture.target,
            &fixture.plan,
            Limits::default(),
            &too_big,
        )
        .expect_err("51 MiB is over the 50 MiB per-file limit");

        assert_eq!(error.exit_code().as_u8(), 26);
        assert_eq!(error.stage(), Stage::Staging);
        assert!(error.to_string().contains("huge.mov"), "{error}");
        assert_eq!(
            fixture.transport.call_count(),
            before,
            "the limit check must run before anything is asked of the host"
        );
    }

    #[test]
    fn too_many_files_is_refused_before_the_batch_directory_is_created() {
        let fixture = fixture();
        let batch: Vec<LocalAttachment> = (0..21)
            .map(|index| attachment(&format!("shot-{index}.png"), 1))
            .collect();

        let before = fixture.transport.call_count();
        let error = stage_attachments(
            &fixture.transport,
            &fixture.target,
            &fixture.plan,
            Limits::default(),
            &batch,
        )
        .expect_err("21 files is one over the limit");

        assert_eq!(error.exit_code().as_u8(), 26);
        assert_eq!(fixture.transport.call_count(), before);
    }

    #[test]
    fn a_batch_within_the_limits_is_created_and_uploaded() {
        let fixture = fixture();
        let batch = [attachment("shot.png", 1024), attachment("notes.txt", 2048)];

        let staged = stage_attachments(
            &fixture.transport,
            &fixture.target,
            &fixture.plan,
            Limits::default(),
            &batch,
        )
        .unwrap();

        assert_eq!(staged.files().len(), 2);
        assert_eq!(staged.directory(), fixture.plan.directory());
    }

    /// The insertion text is the last thing produced, and only if everything
    /// arrived. This is the all-or-nothing rule seen from the caller's side.
    #[test]
    fn a_send_produces_paths_and_text_only_when_every_file_arrived() {
        let transport = RecordingTransport::new("/home/dev");
        let target = TransportTarget::new("core");
        let clock = FakeClock::at_unix_seconds(1_788_093_240);
        let ids = FakeIdSource::starting_at(1);
        let files = [attachment("shot.png", 1024), attachment("notes.txt", 2048)];

        let outcome = perform(
            &transport,
            &target,
            &files,
            &SendPolicy::default(),
            &clock,
            &ids,
        )
        .unwrap();

        assert_eq!(outcome.batch().files().len(), 2);
        assert!(
            outcome
                .insertion_text()
                .starts_with("Please inspect these files:")
        );
        for file in outcome.batch().files() {
            assert!(
                outcome.insertion_text().contains(file.path().as_str()),
                "every path that arrived is in the text"
            );
        }
        assert!(!outcome.insertion_text().ends_with('\n'));
    }

    #[test]
    fn a_send_that_fails_halfway_produces_no_text_at_all() {
        let transport = RecordingTransport::new("/home/dev");
        let clock = FakeClock::at_unix_seconds(1_788_093_240);
        let ids = FakeIdSource::starting_at(1);
        let target = TransportTarget::new("core");

        // The destination of the second file, which needs the same plan the
        // use case will make: same clock, same id source, same inbox.
        let inbox = crate::staging::ensure_inbox(&transport, &target, None).unwrap();
        let plan =
            crate::staging::plan_batch(&inbox, &clock, &FakeIdSource::starting_at(1)).unwrap();
        transport.fail_upload_of(plan.file(&SafeFileName::sanitize("second.txt")).as_str());

        let error = perform(
            &transport,
            &target,
            &[attachment("first.png", 8), attachment("second.txt", 8)],
            &SendPolicy::default(),
            &clock,
            &ids,
        )
        .expect_err("the second upload was made to fail");

        assert_eq!(error.exit_code().as_u8(), 23);
        assert!(
            !error.to_string().contains("Please inspect"),
            "a failure must not carry insertion text: {error}"
        );
    }

    /// An oversized batch must not even open a connection.
    #[test]
    fn an_oversized_send_never_reaches_the_host() {
        let transport = RecordingTransport::new("/home/dev");
        let clock = FakeClock::at_unix_seconds(1_788_093_240);
        let ids = FakeIdSource::starting_at(1);

        let error = perform(
            &transport,
            &TransportTarget::new("core"),
            &[attachment("huge.mov", 51 * 1024 * 1024)],
            &SendPolicy::default(),
            &clock,
            &ids,
        )
        .expect_err("51 MiB is over the limit");

        assert_eq!(error.exit_code().as_u8(), 26);
        assert_eq!(
            transport.call_count(),
            0,
            "not one round trip for a batch that was never going to be sent"
        );
    }

    /// The sweep runs occasionally, and the die is the batch identifier's own
    /// first byte.
    #[test]
    fn the_occasional_sweep_is_occasional() {
        let sweeping = (0..=255u16)
            .filter(|value| should_sweep(&format!("{value:02x}00000000000000000000000000000000")))
            .count();
        assert_eq!(
            sweeping, 31,
            "roughly one identifier in {SWEEP_ONE_IN} should trigger a sweep"
        );
        assert!(should_sweep("00abcdef00000000000000000000abcd"));
        assert!(!should_sweep("ffabcdef00000000000000000000abcd"));
        // An identifier that cannot be read is not a reason to start deleting.
        assert!(!should_sweep(""));
        assert!(!should_sweep("zz"));
    }

    /// The send itself lists nothing: the tidy-up comes after the text has
    /// been delivered, so it can never stand between a key press and a path.
    #[test]
    fn a_send_itself_never_tidies_up() {
        let transport = RecordingTransport::new("/home/dev");
        transport.fail_list_dir("a send must not list anything");
        // The fake identifiers begin with a zero byte, which is on the sweeping
        // side of the line: were the sweep still inside the send, it would run.
        let outcome = perform(
            &transport,
            &TransportTarget::new("core"),
            &[attachment("a.png", 1)],
            &SendPolicy::default(),
            &FakeClock::at_unix_seconds(1_788_093_240),
            &FakeIdSource::starting_at(1),
        )
        .expect("a send that lists nothing cannot fail on a listing");
        assert!(outcome.insertion_text().starts_with("Please inspect"));
    }

    /// The attachments are already there. A failed tidy-up is a note,
    /// not an outcome.
    #[test]
    fn a_failing_tidy_up_is_only_a_note() {
        let transport = RecordingTransport::new("/home/dev");
        let target = TransportTarget::new("core");
        let clock = FakeClock::at_unix_seconds(1_788_093_240);
        let outcome = perform(
            &transport,
            &target,
            &[attachment("a.png", 1)],
            &SendPolicy::default(),
            &clock,
            &FakeIdSource::starting_at(1),
        )
        .unwrap();

        transport.fail_list_dir("the inbox could not be listed");
        let note = tidy_after(
            &outcome,
            &transport,
            &target,
            Some(Duration::from_secs(60)),
            &clock,
        );
        assert!(
            note.as_deref()
                .is_some_and(|note| note.contains("could not tidy up")),
            "the failure is reported as a note: {note:?}"
        );
    }

    /// Without a retention there is nothing to sweep by, so nothing is swept.
    #[test]
    fn no_retention_means_no_sweep() {
        let transport = RecordingTransport::new("/home/dev");
        let target = TransportTarget::new("core");
        let clock = FakeClock::at_unix_seconds(0);
        let outcome = perform(
            &transport,
            &target,
            &[attachment("a.png", 1)],
            &SendPolicy::default(),
            &clock,
            &FakeIdSource::starting_at(1),
        )
        .unwrap();
        transport.fail_list_dir("this must never be reached");
        assert_eq!(
            tidy_after(&outcome, &transport, &target, None, &clock),
            None
        );
    }

    /// A home and cache directory `setup` already recorded are used as they
    /// are, not asked for again.
    #[test]
    fn a_recorded_home_is_not_asked_for_again() {
        let transport = RecordingTransport::new("/home/dev");
        let home = crate::domain::RemotePath::new("/home/dev").unwrap();
        let cache_home = crate::domain::RemotePath::new("/home/dev/.cache").unwrap();
        let outcome = perform(
            &transport,
            &TransportTarget::new("core"),
            &[attachment("a.png", 1)],
            &SendPolicy {
                remote_home: Some(&home),
                remote_cache_home: Some(&cache_home),
                ..SendPolicy::default()
            },
            &FakeClock::at_unix_seconds(1_788_093_240),
            &FakeIdSource::starting_at(1),
        )
        .unwrap();
        assert!(
            outcome
                .insertion_text()
                .contains("/home/dev/.cache/clift/inbox/")
        );
        assert!(
            !transport.calls().iter().any(|call| matches!(
                call,
                crate::testing::TransportCall::ResolveHome { .. }
                    | crate::testing::TransportCall::ResolveCacheHome { .. }
            )),
            "a recorded answer was asked for again: {:?}",
            transport.calls()
        );
    }

    /// Raising the ceiling in configuration must raise it here, or the setting
    /// would be decorative.
    #[test]
    fn a_configured_ceiling_admits_a_batch_the_default_would_refuse() {
        let fixture = fixture();
        let batch = [attachment("huge.mov", 51 * 1024 * 1024)];
        let generous = Limits::new(60 * 1024 * 1024, 120 * 1024 * 1024, 20)
            .unwrap_or_else(|error| panic!("{error}"));

        assert!(
            stage_attachments(
                &fixture.transport,
                &fixture.target,
                &fixture.plan,
                generous,
                &batch,
            )
            .is_ok()
        );
    }
}
