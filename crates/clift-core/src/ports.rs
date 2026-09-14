//! Ports: the boundary between business rules and the outside world.
//!
//! Each trait here is implemented by an adapter crate. The signatures are
//! written so that no adapter detail can leak through them: there is no
//! `Command`, no `Output`, no `NSPasteboard` and no file handle. That is what
//! lets the use cases be tested without a network, a clipboard or a terminal.
//!
//! Every method takes `&self`. A `&mut self` receiver would force one upload at
//! a time into the type system and foreclose the concurrency question
//! before it has been decided.

use crate::context::SshHostSettings;
use crate::domain::{BatchId, RemotePath, SafeFileName};
use crate::error::CliftError;
use crate::universal::ObjectId;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

/// What the clipboard held at the moment it was read.
///
/// Read once, on demand. Clift never watches the clipboard and never keeps a
/// history: a password manager's clipboard content must not pass through Clift
/// unless the user actively asked to send it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ClipboardSnapshot {
    pub text: Option<String>,
    pub files: Vec<PathBuf>,
    pub images: Vec<ClipboardImage>,
}

impl ClipboardSnapshot {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.text.is_none() && self.files.is_empty() && self.images.is_empty()
    }
}

/// An image the clipboard offered, already written to a private temporary file
/// by the adapter.
///
/// The bytes are not carried in memory: the adapter owns the file's lifetime
/// (mode 0600, removed on drop) and `clift-core` performs no local writes of
/// its own.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClipboardImage {
    pub mime: String,
    pub path: PathBuf,
}

/// Reads the system clipboard.
pub trait ClipboardSource {
    /// # Errors
    /// Fails when the clipboard cannot be read at all; an empty clipboard is a
    /// successful read of an empty snapshot, not an error.
    fn read_snapshot(&self) -> Result<ClipboardSnapshot, CliftError>;
}

/// Which host an operation applies to.
///
/// Only the SSH alias: resolving it into a hostname, port, user and jump host
/// is the system OpenSSH client's job, and duplicating that logic is how a tool
/// ends up connecting somewhere the user did not configure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransportTarget {
    ssh_host: String,
}

impl TransportTarget {
    #[must_use]
    pub fn new(ssh_host: impl Into<String>) -> Self {
        Self {
            ssh_host: ssh_host.into(),
        }
    }

    #[must_use]
    pub fn ssh_host(&self) -> &str {
        &self.ssh_host
    }
}

/// Outcome of one diagnostic check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckStatus {
    Pass,
    Warn,
    Fail,
}

/// One line of a probe or doctor report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbeCheck {
    pub name: String,
    pub status: CheckStatus,
    pub detail: String,
}

/// The result of probing a host.
///
/// A failed check does not stop the others: a user with three problems should
/// learn about all three in one run.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProbeReport {
    pub checks: Vec<ProbeCheck>,
}

impl ProbeReport {
    #[must_use]
    pub fn passed(&self) -> bool {
        self.checks
            .iter()
            .all(|check| check.status != CheckStatus::Fail)
    }
}

/// What kind of entry a remote directory listing returned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemoteEntryKind {
    File,
    Directory,
    Symlink,
    Other,
}

/// One entry of a remote directory listing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteEntry {
    pub name: SafeFileName,
    pub kind: RemoteEntryKind,
    pub size: u64,
    /// The permission bits, when the server reports them.
    ///
    /// Present so that a directory which already exists with the wrong
    /// permissions can be refused rather than silently tightened.
    pub mode: Option<u32>,
    /// Needed by retention-based cleanup; absent when the server does not
    /// report it.
    pub modified: Option<SystemTime>,
}

/// Reads the user's own SSH configuration, without interpreting it.
///
/// Separate from [`RemoteFs`] because it opens no connection: it answers "what
/// would `ssh` do with this alias", which `setup` has to show the user *before*
/// anything is dialled.
pub trait SshConfigSource {
    /// The effective settings the system client resolved for an alias.
    ///
    /// # Errors
    /// Fails when the client cannot be run, or when the alias resolves to
    /// nothing usable.
    fn settings_for(&self, alias: &str) -> Result<SshHostSettings, CliftError>;
}

/// Inspects and manages directories on a remote host.
///
/// Split from [`RemoteUpload`] so that a caller asks for what it needs: cleanup
/// never uploads, and a use case that only lists and removes should not be
/// handed the ability to write files (ISP).
///
/// Implemented by driving the system `ssh` and `sftp` executables. No method
/// here may be given an option that weakens host key verification.
pub trait RemoteFs {
    /// Verifies that the host is reachable, authenticated and speaks SFTP.
    ///
    /// # Errors
    /// Fails only when the probe itself could not run; individual failed checks
    /// are reported inside the [`ProbeReport`].
    fn probe(&self, target: &TransportTarget) -> Result<ProbeReport, CliftError>;

    /// Resolves the remote home directory to an absolute path.
    ///
    /// # Errors
    /// Fails when the host cannot be reached or the home cannot be determined.
    fn resolve_home(&self, target: &TransportTarget) -> Result<RemotePath, CliftError>;

    /// The directory the host itself nominates for caches, if it nominates one.
    ///
    /// `Ok(None)` means the host advertised nothing usable as a path: unset,
    /// empty, or not an absolute path. Whether a syntactically fine location is
    /// one Clift is willing to use is not decided here -- that is a policy, and
    /// policies live in `clift-core`.
    ///
    /// # Errors
    /// Fails when the host cannot be reached.
    fn resolve_cache_home(
        &self,
        target: &TransportTarget,
    ) -> Result<Option<RemotePath>, CliftError>;

    /// Creates a directory with exactly `mode`, creating parents as needed.
    ///
    /// # Errors
    /// Fails if the directory exists with different permissions: silently
    /// tightening a directory the user already has would hide a real problem.
    fn ensure_dir(
        &self,
        target: &TransportTarget,
        path: &RemotePath,
        mode: u32,
    ) -> Result<(), CliftError>;

    /// Returns metadata for a path, or `None` if it does not exist.
    ///
    /// # Errors
    /// Fails when the host cannot be reached; a missing path is `Ok(None)`.
    fn stat(
        &self,
        target: &TransportTarget,
        path: &RemotePath,
    ) -> Result<Option<RemoteEntry>, CliftError>;

    /// Lists a directory, without following symbolic links that leave it.
    ///
    /// # Errors
    /// Fails when the directory cannot be listed.
    fn list_dir(
        &self,
        target: &TransportTarget,
        path: &RemotePath,
    ) -> Result<Vec<RemoteEntry>, CliftError>;

    /// Removes a file or an empty directory.
    ///
    /// # Errors
    /// Fails when the path cannot be removed.
    fn remove(&self, target: &TransportTarget, path: &RemotePath) -> Result<(), CliftError>;
}

/// Puts a local file on a remote host, atomically.
pub trait RemoteUpload {
    /// Uploads a local file and returns the number of bytes the remote side
    /// reports afterwards.
    ///
    /// Implementations must upload to a temporary name, verify the size, and
    /// only then rename into place, so that an agent can never read a partial
    /// file.
    ///
    /// # Errors
    /// Fails on transfer errors and on a size mismatch. On failure the
    /// destination must not exist.
    fn upload_atomic(
        &self,
        target: &TransportTarget,
        source: &Path,
        destination: &RemotePath,
    ) -> Result<u64, CliftError>;
}

/// Everything a full send needs from a remote host.
///
/// Nothing implements this directly: it is the pair of capabilities above,
/// named once so that `send` can ask for both without listing them.
pub trait Transport: RemoteFs + RemoteUpload {}

impl<T: RemoteFs + RemoteUpload> Transport for T {}

/// Source of the current time.
///
/// Exists so that "which date directory does this batch go into" can be tested
/// across a midnight boundary. It is not a general abstraction over time and
/// should not grow one.
pub trait Clock {
    fn now(&self) -> SystemTime;
}

/// Source of batch identifiers.
///
/// Exists so that batch isolation can be tested deterministically. Real
/// implementations must draw from the operating system CSPRNG.
pub trait IdSource {
    /// # Errors
    /// Fails when the system random source is unavailable, which must abort the
    /// batch rather than fall back to something predictable.
    fn new_batch_id(&self) -> Result<BatchId, CliftError>;
}

/// Source of raw random bytes.
///
/// Separate from [`IdSource`] rather than a method on it, because the two are
/// asked for by different things: a batch identifier is a domain value with a
/// type, and this is a buffer of entropy that Universal Mode turns into a key
/// and a nonce. Folding them together would mean every existing test double had
/// to grow a method it has no use for.
///
/// Implementations must draw from the operating system CSPRNG and must fail
/// rather than fall back. There is no such thing as a degraded object key.
pub trait Randomness {
    /// Fills `buffer` completely.
    ///
    /// # Errors
    /// Fails when the system random source is unavailable, which must abort
    /// whatever asked for the bytes.
    fn fill(&self, buffer: &mut [u8]) -> Result<(), CliftError>;
}

/// What a relay said about an object it accepted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublishedObject {
    /// The relay's name for the ciphertext. Unpredictable, and chosen by the
    /// relay rather than by the client, so a client cannot claim an id that
    /// somebody else's object would later have been given.
    pub id: ObjectId,
    /// How long the relay says it will keep the object. Reported back to the
    /// user, and never trusted as a guarantee -- an object can also go away
    /// because somebody fetched it.
    pub ttl: Duration,
}

/// Hands ciphertext to a relay and takes it back again.
///
/// Three methods and no more. Everything a relay could usefully be asked to do
/// beyond this -- listing, browsing, authenticating, retaining -- is something
/// The specification rules out, and a port with a method for it would be an invitation.
///
/// **No implementation may be given key material.** The signatures are what
/// enforces that: nothing here takes a [`crate::universal::SealKey`], so there
/// is no relay client that could put one in a query string, a header or a log
/// line even by mistake.
pub trait Relay {
    /// Stores `sealed` and returns the id it was given.
    ///
    /// # Errors
    /// Returns [`crate::error::ErrorKind::RelayUnavailable`] when the relay
    /// cannot be reached or refuses for a reason that is about the relay, and
    /// [`crate::error::ErrorKind::LimitExceeded`] when it refuses because the
    /// object is too large.
    fn publish(&self, sealed: &[u8], ttl: Duration) -> Result<PublishedObject, CliftError>;

    /// Retrieves an object, consuming it.
    ///
    /// A successful retrieval is the object's last: the relay is required to
    /// delete it once the bytes have gone out. That is enforced by the relay
    /// rather than by a follow-up call from the client, because a client that
    /// crashes between the two would otherwise leave the object claimable.
    ///
    /// # Errors
    /// Returns [`crate::error::ErrorKind::TokenUnusable`] when the object is
    /// not there -- expired, already fetched, or never stored -- and
    /// [`crate::error::ErrorKind::RelayUnavailable`] for anything else.
    fn retrieve(&self, id: &ObjectId) -> Result<Vec<u8>, CliftError>;

    /// Withdraws an object that was published but never handed to anyone.
    ///
    /// The case this exists for: the ciphertext went up, and then putting the
    /// token in front of the user failed. Waiting out the TTL would also work,
    /// and this makes the window seconds instead of minutes.
    ///
    /// # Errors
    /// Fails when the relay cannot be reached. An object that is already gone
    /// is a success, because the outcome the caller wanted has been reached.
    fn revoke(&self, id: &ObjectId) -> Result<(), CliftError>;
}
