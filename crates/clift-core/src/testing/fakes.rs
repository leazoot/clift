use crate::context::{SshHostSettings, parse_effective_config};
use crate::domain::SafeFileName;
use crate::domain::{BatchId, RemotePath};
use crate::error::{CliftError, ErrorKind, Stage};
use crate::ports::{
    CheckStatus, ClipboardSnapshot, ClipboardSource, Clock, IdSource, ProbeCheck, ProbeReport,
    PublishedObject, Randomness, Relay, RemoteEntry, RemoteEntryKind, RemoteFs, RemoteUpload,
    SshConfigSource, TransportTarget,
};
use crate::universal::ObjectId;
use crate::universal::token::OBJECT_ID_BYTES;
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime};

/// A clipboard that returns whatever the test put in it.
#[derive(Debug, Default)]
pub struct FakeClipboard {
    snapshot: ClipboardSnapshot,
    reads: AtomicU64,
    fails: bool,
}

impl FakeClipboard {
    #[must_use]
    pub fn with(snapshot: ClipboardSnapshot) -> Self {
        Self {
            snapshot,
            reads: AtomicU64::new(0),
            fails: false,
        }
    }

    /// A clipboard that cannot be read at all, for the failure branch.
    #[must_use]
    pub fn failing() -> Self {
        Self {
            snapshot: ClipboardSnapshot::default(),
            reads: AtomicU64::new(0),
            fails: true,
        }
    }

    /// How many times the clipboard was read. Clift must read it once per
    /// invocation, never poll it.
    #[must_use]
    pub fn reads(&self) -> u64 {
        self.reads.load(Ordering::Relaxed)
    }
}

impl ClipboardSource for FakeClipboard {
    fn read_snapshot(&self) -> Result<ClipboardSnapshot, CliftError> {
        self.reads.fetch_add(1, Ordering::Relaxed);
        if self.fails {
            return Err(CliftError::new(
                Stage::Clipboard,
                ErrorKind::ClipboardRead,
                "the clipboard could not be read",
            ));
        }
        Ok(self.snapshot.clone())
    }
}

/// One call made through [`RecordingTransport`].
///
/// Tests assert on these to prove things that are otherwise invisible, such as
/// "a plain text paste opened no connection at all".
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransportCall {
    Probe { ssh_host: String },
    ResolveHome { ssh_host: String },
    ResolveCacheHome { ssh_host: String },
    EnsureDir { path: String, mode: u32 },
    UploadAtomic { source: String, destination: String },
    Stat { path: String },
    ListDir { path: String },
    Remove { path: String },
}

/// A transport that records every call and never touches a network.
#[derive(Debug)]
pub struct RecordingTransport {
    calls: Mutex<Vec<TransportCall>>,
    home: RemotePath,
    cache_home: Mutex<Option<RemotePath>>,
    fail_upload_of: Mutex<Option<String>>,
    uploaded: Mutex<BTreeMap<String, u64>>,
    probe_checks: Mutex<Vec<ProbeCheck>>,
    fail_ensure_dir: Mutex<Option<String>>,
    fail_list_dir: Mutex<Option<String>>,
}

impl RecordingTransport {
    /// # Panics
    /// Panics if `home` is not a valid absolute remote path, which would be a
    /// mistake in the test rather than in the code under test.
    #[must_use]
    pub fn new(home: &str) -> Self {
        Self {
            calls: Mutex::new(Vec::new()),
            home: RemotePath::new(home)
                .unwrap_or_else(|error| panic!("invalid fake home {home:?}: {error}")),
            cache_home: Mutex::new(None),
            fail_upload_of: Mutex::new(None),
            uploaded: Mutex::new(BTreeMap::new()),
            probe_checks: Mutex::new(Vec::new()),
            fail_ensure_dir: Mutex::new(None),
            fail_list_dir: Mutex::new(None),
        }
    }

    /// Makes the host advertise a cache directory of its own.
    ///
    /// # Panics
    /// Panics if `path` is not a valid absolute remote path.
    pub fn advertise_cache_home(&self, path: &str) {
        let parsed = RemotePath::new(path)
            .unwrap_or_else(|error| panic!("invalid fake cache home {path:?}: {error}"));
        if let Ok(mut slot) = self.cache_home.lock() {
            *slot = Some(parsed);
        }
    }

    /// Makes the upload of one destination path fail, for testing that a
    /// partially failed batch produces no insertion text at all.
    pub fn fail_upload_of(&self, destination: &str) {
        if let Ok(mut slot) = self.fail_upload_of.lock() {
            *slot = Some(destination.to_string());
        }
    }

    /// Makes the probe report one named check with a chosen status, so that a
    /// diagnostic's failure branch can be exercised.
    pub fn report_check(&self, name: &str, status: CheckStatus, detail: &str) {
        if let Ok(mut checks) = self.probe_checks.lock() {
            checks.push(ProbeCheck {
                name: name.to_string(),
                status,
                detail: detail.to_string(),
            });
        }
    }

    /// Makes every `list_dir` fail, as an inbox that cannot be read would.
    pub fn fail_list_dir(&self, reason: &str) {
        if let Ok(mut slot) = self.fail_list_dir.lock() {
            *slot = Some(reason.to_string());
        }
    }

    /// Makes every `ensure_dir` fail, as an unwritable remote home would.
    pub fn fail_ensure_dir(&self, reason: &str) {
        if let Ok(mut slot) = self.fail_ensure_dir.lock() {
            *slot = Some(reason.to_string());
        }
    }

    #[must_use]
    pub fn calls(&self) -> Vec<TransportCall> {
        self.calls
            .lock()
            .map(|calls| calls.clone())
            .unwrap_or_default()
    }

    #[must_use]
    pub fn call_count(&self) -> usize {
        self.calls.lock().map(|calls| calls.len()).unwrap_or(0)
    }

    fn record(&self, call: TransportCall) {
        if let Ok(mut calls) = self.calls.lock() {
            calls.push(call);
        }
    }
}

impl RemoteFs for RecordingTransport {
    fn probe(&self, target: &TransportTarget) -> Result<ProbeReport, CliftError> {
        self.record(TransportCall::Probe {
            ssh_host: target.ssh_host().to_string(),
        });
        Ok(ProbeReport {
            checks: self
                .probe_checks
                .lock()
                .map(|checks| checks.clone())
                .unwrap_or_default(),
        })
    }

    fn resolve_home(&self, target: &TransportTarget) -> Result<RemotePath, CliftError> {
        self.record(TransportCall::ResolveHome {
            ssh_host: target.ssh_host().to_string(),
        });
        Ok(self.home.clone())
    }

    fn resolve_cache_home(
        &self,
        target: &TransportTarget,
    ) -> Result<Option<RemotePath>, CliftError> {
        self.record(TransportCall::ResolveCacheHome {
            ssh_host: target.ssh_host().to_string(),
        });
        Ok(self.cache_home.lock().ok().and_then(|slot| slot.clone()))
    }

    fn ensure_dir(
        &self,
        _target: &TransportTarget,
        path: &RemotePath,
        mode: u32,
    ) -> Result<(), CliftError> {
        self.record(TransportCall::EnsureDir {
            path: path.as_str().to_string(),
            mode,
        });
        if let Some(reason) = self
            .fail_ensure_dir
            .lock()
            .ok()
            .and_then(|slot| slot.clone())
        {
            return Err(CliftError::new(
                Stage::Staging,
                ErrorKind::RemoteDirectory,
                reason,
            ));
        }
        Ok(())
    }

    fn stat(
        &self,
        _target: &TransportTarget,
        path: &RemotePath,
    ) -> Result<Option<RemoteEntry>, CliftError> {
        self.record(TransportCall::Stat {
            path: path.as_str().to_string(),
        });
        // Uploads are remembered so that "upload it, look at it, remove it,
        // look again" behaves the way it does against a real host. Anything
        // never uploaded is absent, which is what a fresh host looks like.
        Ok(self
            .uploaded
            .lock()
            .ok()
            .and_then(|files| files.get(path.as_str()).copied())
            .map(|size| RemoteEntry {
                name: base_name(path),
                kind: RemoteEntryKind::File,
                size,
                mode: Some(0o600),
                modified: None,
            }))
    }

    fn list_dir(
        &self,
        _target: &TransportTarget,
        path: &RemotePath,
    ) -> Result<Vec<RemoteEntry>, CliftError> {
        self.record(TransportCall::ListDir {
            path: path.as_str().to_string(),
        });
        if let Some(reason) = self.fail_list_dir.lock().ok().and_then(|slot| slot.clone()) {
            return Err(CliftError::new(
                Stage::Staging,
                ErrorKind::RemoteDirectory,
                reason,
            ));
        }
        Ok(Vec::new())
    }

    fn remove(&self, _target: &TransportTarget, path: &RemotePath) -> Result<(), CliftError> {
        self.record(TransportCall::Remove {
            path: path.as_str().to_string(),
        });
        if let Ok(mut files) = self.uploaded.lock() {
            files.remove(path.as_str());
        }
        Ok(())
    }
}

impl RemoteUpload for RecordingTransport {
    fn upload_atomic(
        &self,
        _target: &TransportTarget,
        source: &Path,
        destination: &RemotePath,
    ) -> Result<u64, CliftError> {
        self.record(TransportCall::UploadAtomic {
            source: source.display().to_string(),
            destination: destination.as_str().to_string(),
        });
        let should_fail = self
            .fail_upload_of
            .lock()
            .ok()
            .and_then(|slot| slot.clone())
            .is_some_and(|wanted| wanted == destination.as_str());
        if should_fail {
            return Err(CliftError::new(
                Stage::Transfer,
                ErrorKind::Transfer,
                format!("injected upload failure for {destination}"),
            ));
        }
        if let Ok(mut files) = self.uploaded.lock() {
            files.insert(
                destination.as_str().to_string(),
                std::fs::metadata(source)
                    .map(|meta| meta.len())
                    .unwrap_or(0),
            );
        }
        // The real transport reports what the host says arrived. A fake that
        // always said zero would let a size check pass here and fail in
        // production, so it reads the local file it was actually given.
        Ok(std::fs::metadata(source)
            .map(|meta| meta.len())
            .unwrap_or(0))
    }
}

fn base_name(path: &RemotePath) -> SafeFileName {
    SafeFileName::sanitize(path.as_str().rsplit('/').next().unwrap_or("attachment"))
}

/// A clock frozen at a chosen instant, so that date-directory behaviour around
/// midnight can be tested without waiting for midnight.
#[derive(Debug)]
pub struct FakeClock {
    now: Mutex<SystemTime>,
}

impl FakeClock {
    #[must_use]
    pub fn at_unix_seconds(seconds: u64) -> Self {
        Self {
            now: Mutex::new(SystemTime::UNIX_EPOCH + Duration::from_secs(seconds)),
        }
    }

    pub fn advance(&self, by: Duration) {
        if let Ok(mut now) = self.now.lock() {
            *now += by;
        }
    }
}

impl Clock for FakeClock {
    fn now(&self) -> SystemTime {
        self.now
            .lock()
            .map(|now| *now)
            .unwrap_or(SystemTime::UNIX_EPOCH)
    }
}

/// An identifier source that counts up, so batch directories are predictable in
/// tests and only in tests.
#[derive(Debug, Default)]
pub struct FakeIdSource {
    next: AtomicU64,
}

impl FakeIdSource {
    #[must_use]
    pub fn starting_at(first: u64) -> Self {
        Self {
            next: AtomicU64::new(first),
        }
    }
}

impl IdSource for FakeIdSource {
    fn new_batch_id(&self) -> Result<BatchId, CliftError> {
        let value = self.next.fetch_add(1, Ordering::Relaxed).wrapping_add(1);
        let mut bytes = [0u8; 16];
        bytes[8..].copy_from_slice(&value.to_be_bytes());
        BatchId::from_random_bytes(bytes)
            .map_err(|error| error.into_clift(Stage::Staging, ErrorKind::Internal))
    }
}

/// An SSH configuration that answers from a canned `ssh -G` reply.
#[derive(Debug)]
pub struct FakeSshConfig {
    answer: Option<String>,
}

impl FakeSshConfig {
    /// Resolves every alias to the given user, host and port.
    #[must_use]
    pub fn resolving(user: &str, host_name: &str, port: u16) -> Self {
        Self {
            answer: Some(format!("user {user}\nhostname {host_name}\nport {port}\n")),
        }
    }

    /// An alias the local client cannot make sense of.
    #[must_use]
    pub fn failing() -> Self {
        Self { answer: None }
    }
}

impl SshConfigSource for FakeSshConfig {
    fn settings_for(&self, alias: &str) -> Result<SshHostSettings, CliftError> {
        match &self.answer {
            Some(answer) => parse_effective_config(alias, answer),
            None => Err(CliftError::new(
                Stage::Config,
                ErrorKind::Config,
                format!("ssh could not resolve {alias}"),
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ports::ClipboardImage;
    use std::path::PathBuf;
    use std::thread;

    fn target() -> TransportTarget {
        TransportTarget::new("core")
    }

    fn path(value: &str) -> RemotePath {
        RemotePath::new(value).unwrap()
    }

    /// The port signatures must permit a concurrent implementation later
    ///. If `Transport` needed `&mut self`, this would not compile.
    #[test]
    fn a_transport_can_be_shared_across_threads() {
        fn assert_shareable<T: crate::ports::Transport + Send + Sync>(_: &T) {}

        let transport = RecordingTransport::new("/home/dev");
        assert_shareable(&transport);

        thread::scope(|scope| {
            for index in 0..4 {
                let transport = &transport;
                scope.spawn(move || {
                    let directory = path(&format!("/home/dev/inbox/{index}"));
                    transport.ensure_dir(&target(), &directory, 0o700).unwrap();
                });
            }
        });

        assert_eq!(transport.call_count(), 4);
    }

    #[test]
    fn the_recording_transport_reports_what_was_asked_of_it() {
        let transport = RecordingTransport::new("/home/dev");
        let inbox = path("/home/dev/.cache/clift/inbox");

        assert_eq!(
            transport.resolve_home(&target()).unwrap(),
            path("/home/dev")
        );
        transport.ensure_dir(&target(), &inbox, 0o700).unwrap();
        transport
            .upload_atomic(&target(), Path::new("/tmp/a.png"), &inbox)
            .unwrap();

        assert_eq!(
            transport.calls(),
            vec![
                TransportCall::ResolveHome {
                    ssh_host: "core".to_string()
                },
                TransportCall::EnsureDir {
                    path: "/home/dev/.cache/clift/inbox".to_string(),
                    mode: 0o700,
                },
                TransportCall::UploadAtomic {
                    source: "/tmp/a.png".to_string(),
                    destination: "/home/dev/.cache/clift/inbox".to_string(),
                },
            ]
        );
    }

    /// The guard behind the all-or-nothing rule: a batch in which one upload fails must be
    /// distinguishable from one that succeeded.
    #[test]
    fn an_injected_upload_failure_is_reported_as_a_transfer_error() {
        let transport = RecordingTransport::new("/home/dev");
        let good = path("/home/dev/inbox/a.png");
        let bad = path("/home/dev/inbox/b.png");
        transport.fail_upload_of(bad.as_str());

        assert!(
            transport
                .upload_atomic(&target(), Path::new("/tmp/a.png"), &good)
                .is_ok()
        );
        let error = transport
            .upload_atomic(&target(), Path::new("/tmp/b.png"), &bad)
            .unwrap_err();
        assert_eq!(error.exit_code().as_u8(), 23);
    }

    #[test]
    fn the_clipboard_is_read_once_per_call_and_never_polled() {
        let clipboard = FakeClipboard::with(ClipboardSnapshot {
            text: Some("hello".to_string()),
            files: vec![PathBuf::from("/tmp/a.png")],
            images: vec![ClipboardImage {
                mime: "image/png".to_string(),
                path: PathBuf::from("/tmp/shot.png"),
            }],
        });

        assert_eq!(clipboard.reads(), 0);
        let snapshot = clipboard.read_snapshot().unwrap();
        assert_eq!(clipboard.reads(), 1);
        assert!(!snapshot.is_empty());
        assert_eq!(snapshot.files.len(), 1);
        assert_eq!(snapshot.images[0].mime, "image/png");
    }

    #[test]
    fn an_empty_snapshot_is_recognised_as_empty() {
        let clipboard = FakeClipboard::default();
        assert!(clipboard.read_snapshot().unwrap().is_empty());
    }

    #[test]
    fn the_clock_only_moves_when_a_test_moves_it() {
        let clock = FakeClock::at_unix_seconds(1_772_000_000);
        let first = clock.now();
        assert_eq!(clock.now(), first, "a frozen clock must not drift");
        clock.advance(Duration::from_secs(60));
        assert_eq!(
            clock.now().duration_since(first).unwrap(),
            Duration::from_secs(60)
        );
    }

    #[test]
    fn the_fake_id_source_yields_distinct_ids() {
        let ids = FakeIdSource::default();
        let first = ids.new_batch_id().unwrap();
        let second = ids.new_batch_id().unwrap();
        assert_ne!(first, second);
        assert_eq!(first.as_str().len(), 32);
    }
}

/// A random source whose output is predictable, so a sealed object is
/// reproducible in a test.
///
/// Predictability is the point and also the danger: this must never be reachable
/// from shipped code, which is what the `testing` feature and the architecture
/// check between them guarantee. Each call still returns something different
/// from the last, because tests that assert "two objects differ" would otherwise
/// pass for the wrong reason.
#[derive(Debug, Default)]
pub struct CountingRandomness {
    next: AtomicU64,
}

impl CountingRandomness {
    #[must_use]
    pub fn new() -> Self {
        Self {
            next: AtomicU64::new(1),
        }
    }
}

impl Randomness for CountingRandomness {
    fn fill(&self, buffer: &mut [u8]) -> Result<(), CliftError> {
        let seed = self.next.fetch_add(1, Ordering::Relaxed);
        for (index, byte) in buffer.iter_mut().enumerate() {
            let mixed = seed
                .wrapping_mul(0x9E37_79B9_7F4A_7C15)
                .wrapping_add(index as u64);
            *byte = u8::try_from(mixed >> 33 & 0xFF).unwrap_or(0);
        }
        Ok(())
    }
}

/// A machine with no working random source. There is no correct way to proceed
/// from here, and the test that uses this asserts exactly that.
#[derive(Debug)]
pub struct FailingRandomness;

impl Randomness for FailingRandomness {
    fn fill(&self, _buffer: &mut [u8]) -> Result<(), CliftError> {
        Err(CliftError::new(
            Stage::Internal,
            ErrorKind::Internal,
            "the operating system random source is unavailable",
        ))
    }
}

/// A relay held in memory, with the single-use rule enforced the way a real one
/// enforces it.
///
/// It records the calls made to it so a test can assert what was *not* sent:
/// the whole security argument for Universal Mode rests on key material never
/// reaching this side, and an assertion is the only way that stays true.
#[derive(Debug)]
pub struct RecordingRelay {
    objects: Mutex<BTreeMap<String, Vec<u8>>>,
    calls: Mutex<Vec<String>>,
    ids: AtomicU64,
    available: bool,
}

impl Default for RecordingRelay {
    fn default() -> Self {
        Self::new()
    }
}

impl RecordingRelay {
    #[must_use]
    pub fn new() -> Self {
        Self {
            objects: Mutex::new(BTreeMap::new()),
            calls: Mutex::new(Vec::new()),
            ids: AtomicU64::new(1),
            available: true,
        }
    }

    /// A relay that cannot be reached at all.
    #[must_use]
    pub fn unavailable() -> Self {
        Self {
            available: false,
            ..Self::new()
        }
    }

    /// Everything asked of this relay, rendered as text, for assertions about
    /// what never appears in it.
    #[must_use]
    pub fn recorded_calls(&self) -> Vec<String> {
        self.calls
            .lock()
            .map(|calls| calls.clone())
            .unwrap_or_default()
    }

    /// The bytes of the single object this relay holds.
    ///
    /// Panics if there is not exactly one; every test using it publishes once.
    #[must_use]
    pub fn stored_bytes(&self) -> Vec<u8> {
        let objects = self
            .objects
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        assert_eq!(objects.len(), 1, "expected exactly one stored object");
        objects.values().next().cloned().unwrap_or_default()
    }

    /// Flips a byte of the stored ciphertext, standing in for a relay that
    /// alters what it was given.
    pub fn tamper_with_stored_object(&self) {
        let mut objects = self
            .objects
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        for bytes in objects.values_mut() {
            if let Some(last) = bytes.last_mut() {
                *last ^= 0x01;
            }
        }
    }

    fn unreachable(&self) -> CliftError {
        CliftError::new(
            Stage::Relay,
            ErrorKind::RelayUnavailable,
            "the relay could not be reached",
        )
    }
}

impl Relay for RecordingRelay {
    fn publish(&self, sealed: &[u8], ttl: Duration) -> Result<PublishedObject, CliftError> {
        self.calls
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push(format!(
                "publish {} bytes ttl={}s",
                sealed.len(),
                ttl.as_secs()
            ));
        if !self.available {
            return Err(self.unreachable());
        }
        let mut bytes = [0_u8; OBJECT_ID_BYTES];
        bytes[8..].copy_from_slice(&self.ids.fetch_add(1, Ordering::Relaxed).to_be_bytes());
        let id = ObjectId::from_bytes(bytes);
        self.objects
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(id.encoded(), sealed.to_vec());
        Ok(PublishedObject { id, ttl })
    }

    fn retrieve(&self, id: &ObjectId) -> Result<Vec<u8>, CliftError> {
        self.calls
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push(format!("retrieve {id}"));
        if !self.available {
            return Err(self.unreachable());
        }
        // Removed on the way out, which is what makes the object single use.
        self.objects
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(&id.encoded())
            .ok_or_else(|| {
                CliftError::new(
                    Stage::Relay,
                    ErrorKind::TokenUnusable,
                    "the relay has no object for this token: it expired, or it was already fetched",
                )
            })
    }

    fn revoke(&self, id: &ObjectId) -> Result<(), CliftError> {
        self.calls
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push(format!("revoke {id}"));
        if !self.available {
            return Err(self.unreachable());
        }
        self.objects
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(&id.encoded());
        Ok(())
    }
}
