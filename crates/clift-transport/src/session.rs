//! One SFTP session over the system `ssh`, spoken in the protocol itself.
//!
//! `ssh -s <host> sftp` asks the server for its SFTP subsystem and then carries
//! bytes both ways. Everything to do with reaching and trusting the host stays
//! with `ssh`: the configuration, known_hosts, the agent, hardware keys,
//! ProxyJump. What travels over the pipes is SFTP version 3 ([`crate::wire`]),
//! which Clift writes and reads directly.
//!
//! # Why not the `sftp` program
//!
//! Clift used to drive `sftp -b -` and read what it printed. That meant
//! parsing text meant for people: permission columns, dates in the local time
//! zone, echoed commands used as frame markers. On Windows it did not work at
//! all as a session, because that build of `sftp` holds everything it writes
//! to a pipe until it exits, so every operation became a new process and a new
//! authentication: 55 seconds for one screenshot on a link where `ssh` itself
//! takes under four. `ssh` passes the subsystem's bytes on as they arrive, on
//! every platform, so one connection carries a whole run.
//!
//! # Timeouts
//!
//! Requests are written by a thread of their own and replies are read by
//! another, so neither direction can block the caller. The caller waits for a
//! reply with a limit; when the limit passes, `ssh` is stopped, which also ends
//! any write that was stuck behind it. A session that has timed out or lost its
//! connection is finished and must be dropped. Nothing here retries: whether a
//! request that was already written reached the server cannot be known, and a
//! `rename` sent twice is exactly what the specification forbids.

use crate::wire::{self, Attrs, NameEntry, Reply};
use std::collections::{HashMap, VecDeque};
use std::ffi::OsString;
use std::fmt;
use std::io::{Read, Write};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

/// Writes in flight at once during an upload. The same figure OpenSSH's own
/// client uses: enough that a distant host is not waited on write by write,
/// few enough that a refusal is noticed within a couple of megabytes.
const WRITE_WINDOW: usize = 64;

/// How long a finished `ssh` may take to hand over the last of its stderr.
const STDERR_GRACE: Duration = Duration::from_secs(2);

const POLL_INTERVAL: Duration = Duration::from_millis(5);

/// The server declined a request, in its own words.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refusal {
    pub code: u32,
    pub message: String,
}

/// Why a request did not get the answer it asked for.
#[derive(Debug)]
pub enum Failure {
    /// The server answered, and the answer was no.
    Refused(Refusal),
    /// The connection could not carry the request. The session is finished.
    Broken {
        reason: String,
        /// What `ssh` printed, which is where the actual cause usually is.
        stderr: String,
        timed_out: bool,
    },
    /// The local file being uploaded could not be read.
    Local(std::io::Error),
    /// `ssh` itself could not be started.
    NotStarted(std::io::Error),
}

impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Failure::Refused(refusal) => write!(f, "{}", refusal.message),
            Failure::Broken { reason, .. } => f.write_str(reason),
            Failure::Local(error) | Failure::NotStarted(error) => write!(f, "{error}"),
        }
    }
}

/// A live `ssh -s <host> sftp`.
#[derive(Debug)]
pub struct SftpSession {
    child: Child,
    requests: Option<Sender<Vec<u8>>>,
    replies: Receiver<Result<Vec<u8>, String>>,
    stderr: Arc<Mutex<Vec<u8>>>,
    stderr_reader: JoinHandle<()>,
    /// Replies that arrived while a different one was being waited for.
    early: HashMap<u32, Reply>,
    next_id: u32,
    timeout: Duration,
    /// When the operation in progress must have finished by.
    deadline: Instant,
    broken: bool,
}

impl SftpSession {
    /// Starts `program` with `args` and completes the SFTP greeting.
    ///
    /// The greeting is where the connection is made, so this is also where a
    /// rejected key, a changed host key or a missing subsystem shows up, in
    /// `ssh`'s own words on [`Failure::Broken`].
    ///
    /// # Errors
    /// Fails when the process cannot be started, when the connection fails,
    /// and when the server does not speak version 3.
    pub fn open(program: &Path, args: &[OsString], timeout: Duration) -> Result<Self, Failure> {
        let mut child = Command::new(program)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(Failure::NotStarted)?;

        let (Some(stdin), Some(stdout), Some(stderr)) =
            (child.stdin.take(), child.stdout.take(), child.stderr.take())
        else {
            let _ = child.kill();
            let _ = child.wait();
            return Err(Failure::Broken {
                reason: "the ssh client did not provide the expected pipes".to_string(),
                stderr: String::new(),
                timed_out: false,
            });
        };

        let requests = spawn_writer(stdin);
        let replies = spawn_reader(stdout);
        let captured = Arc::new(Mutex::new(Vec::new()));
        let stderr_reader = spawn_stderr(stderr, Arc::clone(&captured));

        let mut session = Self {
            child,
            requests: Some(requests),
            replies,
            stderr: captured,
            stderr_reader,
            early: HashMap::new(),
            next_id: 0,
            timeout,
            deadline: Instant::now() + timeout,
            broken: false,
        };
        session.send(wire::init())?;
        let body = session.next_packet()?;
        let version = wire::decode_version(&body).map_err(|error| session.protocol(&error))?;
        if version != wire::VERSION {
            return Err(session.fail(
                format!(
                    "the server speaks SFTP version {version}, and Clift speaks version {}",
                    wire::VERSION
                ),
                false,
            ));
        }
        Ok(session)
    }

    /// Starts the clock for one operation.
    ///
    /// The limit covers the operation as a whole, the way it once covered a
    /// whole `sftp` process, rather than each reply: a transfer that keeps
    /// making progress but will not finish in time is stopped just the same.
    pub fn start_operation(&mut self) {
        self.start_operation_within(self.timeout);
    }

    /// Starts the clock for one operation with a limit of its own, for a
    /// request that must not keep anyone waiting as long as a transfer may.
    pub fn start_operation_within(&mut self, limit: Duration) {
        self.deadline = Instant::now() + limit;
    }

    /// Whether the session can still be used: nothing has gone wrong on it,
    /// and `ssh` has not exited in the meantime.
    pub fn is_usable(&mut self) -> bool {
        !self.broken && matches!(self.child.try_wait(), Ok(None))
    }

    /// The absolute form of `path`, as the server resolves it.
    ///
    /// # Errors
    /// Fails when the server refuses or the connection fails.
    pub fn realpath(&mut self, path: &str) -> Result<Vec<u8>, Failure> {
        let id = self.id();
        self.send(wire::realpath(id, path))?;
        let mut entries = self.expect_name(id)?;
        if entries.len() != 1 {
            return Err(self.fail(
                format!("the server resolved one path into {} names", entries.len()),
                false,
            ));
        }
        Ok(entries.remove(0).filename)
    }

    /// Metadata for `path` without following a link at its end, or `None`
    /// when nothing is there.
    ///
    /// # Errors
    /// Fails when the server refuses for any other reason, or the connection
    /// fails.
    pub fn lstat(&mut self, path: &str) -> Result<Option<Attrs>, Failure> {
        let id = self.id();
        self.send(wire::lstat(id, path))?;
        absent_as_none(self.expect_attrs(id))
    }

    /// Creates a directory asking for `mode`, and reads back what is at
    /// `path` afterwards, in one round trip.
    ///
    /// The two answers together settle what a single one cannot: whether
    /// this call created the directory or found something already there, and
    /// what its mode is either way.
    ///
    /// # Errors
    /// Fails when the connection fails, or the read-back is refused for a
    /// reason other than the path not existing.
    pub fn mkdir_and_lstat(
        &mut self,
        path: &str,
        mode: u32,
    ) -> Result<(Result<(), Refusal>, Option<Attrs>), Failure> {
        let made = self.id();
        let looked = self.id();
        self.send(wire::mkdir(made, path, mode))?;
        self.send(wire::lstat(looked, path))?;
        let created = match self.expect_ok(made) {
            Ok(()) => Ok(()),
            Err(Failure::Refused(refusal)) => Err(refusal),
            Err(other) => return Err(other),
        };
        let attrs = absent_as_none(self.expect_attrs(looked))?;
        Ok((created, attrs))
    }

    /// Sets the permissions of `path`.
    ///
    /// # Errors
    /// Fails when the server refuses or the connection fails.
    pub fn set_mode(&mut self, path: &str, mode: u32) -> Result<(), Failure> {
        let id = self.id();
        self.send(wire::setstat_mode(id, path, mode))?;
        self.expect_ok(id)
    }

    /// Every entry of a directory, `.` and `..` included, or `None` when the
    /// directory does not exist.
    ///
    /// # Errors
    /// Fails when the server refuses for another reason or the connection
    /// fails.
    pub fn list(&mut self, path: &str) -> Result<Option<Vec<NameEntry>>, Failure> {
        let id = self.id();
        self.send(wire::opendir(id, path))?;
        let Some(handle) = absent_as_none(self.expect_handle(id))? else {
            return Ok(None);
        };

        let mut all = Vec::new();
        let listed = loop {
            let id = self.id();
            self.send(wire::readdir(id, &handle))?;
            match self.expect_name(id) {
                Ok(mut entries) => all.append(&mut entries),
                Err(Failure::Refused(refusal)) if refusal.code == wire::status::EOF => {
                    break Ok(());
                }
                Err(other) => break Err(other),
            }
        };
        let closed = self.close(&handle);
        listed?;
        closed?;
        Ok(Some(all))
    }

    /// Removes a file or a symbolic link, never following it.
    ///
    /// # Errors
    /// Fails when the server refuses or the connection fails.
    pub fn remove(&mut self, path: &str) -> Result<(), Failure> {
        let id = self.id();
        self.send(wire::remove(id, path))?;
        self.expect_ok(id)
    }

    /// Removes an empty directory.
    ///
    /// # Errors
    /// Fails when the server refuses or the connection fails.
    pub fn rmdir(&mut self, path: &str) -> Result<(), Failure> {
        let id = self.id();
        self.send(wire::rmdir(id, path))?;
        self.expect_ok(id)
    }

    /// Renames `from` to `to`, refusing to replace anything at `to`.
    ///
    /// # Errors
    /// Fails when the server refuses or the connection fails.
    pub fn rename(&mut self, from: &str, to: &str) -> Result<(), Failure> {
        let id = self.id();
        self.send(wire::rename(id, from, to))?;
        self.expect_ok(id)
    }

    /// Writes `source` to a new file at `path` with `mode`, and returns the
    /// metadata the server reports for it once every byte is acknowledged.
    ///
    /// The file is created exclusively, its mode is set on the open handle
    /// before the first byte is written, and the handle is closed whether or
    /// not the writes succeeded. The first refusal is the one reported.
    ///
    /// # Errors
    /// Fails when the file cannot be created, a write is refused, the local
    /// file cannot be read, or the connection fails.
    pub fn upload(
        &mut self,
        source: &mut dyn Read,
        path: &str,
        mode: u32,
    ) -> Result<Attrs, Failure> {
        let id = self.id();
        self.send(wire::open_exclusive(id, path, mode))?;
        let handle = self.expect_handle(id)?;

        match self.write_all(&handle, source, mode) {
            Ok(()) => {
                let looked = self.id();
                let closed = self.id();
                self.send(wire::fstat(looked, &handle))?;
                self.send(wire::close(closed, &handle))?;
                let attrs = self.expect_attrs(looked);
                let close = self.expect_ok(closed);
                let attrs = attrs?;
                close?;
                Ok(attrs)
            }
            Err(failure @ Failure::Broken { .. }) => Err(failure),
            Err(failure) => {
                // The refusal is what the caller needs. The handle is still
                // released so the server can let go of the file, and whatever
                // the close says is secondary to why it was needed.
                let _ = self.close(&handle);
                Err(failure)
            }
        }
    }

    /// Sends the mode and every chunk of `source`, keeping at most
    /// [`WRITE_WINDOW`] requests unanswered.
    fn write_all(
        &mut self,
        handle: &[u8],
        source: &mut dyn Read,
        mode: u32,
    ) -> Result<(), Failure> {
        let mut pending = VecDeque::new();
        let first = self.id();
        self.send(wire::fsetstat_mode(first, handle, mode))?;
        pending.push_back(first);

        let mut buffer = vec![0_u8; wire::WRITE_CHUNK];
        let mut offset = 0_u64;
        let mut finished = false;
        let mut failure = None;

        loop {
            while failure.is_none() && !finished && pending.len() < WRITE_WINDOW {
                let read = match fill(source, &mut buffer) {
                    Ok(read) => read,
                    Err(error) => {
                        failure = Some(Failure::Local(error));
                        break;
                    }
                };
                if read > 0 {
                    let id = self.id();
                    self.send(wire::write(id, handle, offset, &buffer[..read]))?;
                    pending.push_back(id);
                    offset += read as u64;
                }
                finished = read < buffer.len();
            }
            let Some(id) = pending.pop_front() else {
                break;
            };
            match self.expect_ok(id) {
                Ok(()) => {}
                Err(broken @ Failure::Broken { .. }) => return Err(broken),
                // Stop sending, but collect the answers already owed: they are
                // in the stream whether or not anyone wants them.
                Err(refused) => {
                    if failure.is_none() {
                        failure = Some(refused);
                    }
                }
            }
        }
        failure.map_or(Ok(()), Err)
    }

    fn close(&mut self, handle: &[u8]) -> Result<(), Failure> {
        let id = self.id();
        self.send(wire::close(id, handle))?;
        self.expect_ok(id)
    }

    fn id(&mut self) -> u32 {
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1);
        id
    }

    fn send(&mut self, packet: Vec<u8>) -> Result<(), Failure> {
        if self.broken {
            return Err(self.fail("the SFTP session has already failed".to_string(), true));
        }
        let delivered = self
            .requests
            .as_ref()
            .is_some_and(|requests| requests.send(packet).is_ok());
        if delivered {
            Ok(())
        } else {
            Err(self.fail(
                "the connection closed while a request was being sent".to_string(),
                true,
            ))
        }
    }

    fn next_packet(&mut self) -> Result<Vec<u8>, Failure> {
        let left = self.deadline.saturating_duration_since(Instant::now());
        match self.replies.recv_timeout(left) {
            Ok(Ok(body)) => Ok(body),
            Ok(Err(reason)) => Err(self.fail(reason, true)),
            Err(RecvTimeoutError::Disconnected) => {
                Err(self.fail("the connection closed".to_string(), true))
            }
            Err(RecvTimeoutError::Timeout) => {
                let _ = self.child.kill();
                let reason = format!(
                    "the server did not answer within {} seconds",
                    self.timeout.as_secs()
                );
                let mut failure = self.fail(reason, true);
                if let Failure::Broken { timed_out, .. } = &mut failure {
                    *timed_out = true;
                }
                Err(failure)
            }
        }
    }

    fn wait(&mut self, id: u32) -> Result<Reply, Failure> {
        if let Some(reply) = self.early.remove(&id) {
            return Ok(reply);
        }
        loop {
            let body = self.next_packet()?;
            let reply = wire::decode_reply(&body).map_err(|error| self.protocol(&error))?;
            if reply.id() == id {
                return Ok(reply);
            }
            self.early.insert(reply.id(), reply);
        }
    }

    fn expect_ok(&mut self, id: u32) -> Result<(), Failure> {
        match self.wait(id)? {
            Reply::Status { code, .. } if code == wire::status::OK => Ok(()),
            Reply::Status { code, message, .. } => Err(Failure::Refused(Refusal { code, message })),
            other => Err(self.unexpected(&other)),
        }
    }

    fn expect_handle(&mut self, id: u32) -> Result<Vec<u8>, Failure> {
        match self.wait(id)? {
            Reply::Handle { handle, .. } => Ok(handle),
            other => Err(self.refusal_or_unexpected(other)),
        }
    }

    fn expect_attrs(&mut self, id: u32) -> Result<Attrs, Failure> {
        match self.wait(id)? {
            Reply::Attrs { attrs, .. } => Ok(attrs),
            other => Err(self.refusal_or_unexpected(other)),
        }
    }

    fn expect_name(&mut self, id: u32) -> Result<Vec<NameEntry>, Failure> {
        match self.wait(id)? {
            Reply::Name { entries, .. } => Ok(entries),
            other => Err(self.refusal_or_unexpected(other)),
        }
    }

    fn refusal_or_unexpected(&mut self, reply: Reply) -> Failure {
        match reply {
            Reply::Status { code, message, .. } if code != wire::status::OK => {
                Failure::Refused(Refusal { code, message })
            }
            other => self.unexpected(&other),
        }
    }

    fn unexpected(&mut self, reply: &Reply) -> Failure {
        self.fail(
            format!(
                "the server answered request {} with a reply of the wrong kind",
                reply.id()
            ),
            false,
        )
    }

    fn protocol(&mut self, error: &wire::WireError) -> Failure {
        self.fail(error.to_string(), false)
    }

    /// Marks the session finished and gathers what `ssh` said about it.
    ///
    /// When the connection ended by itself, `ssh` is on its way out and the
    /// reason is still arriving on stderr, so it is given a moment to finish.
    /// Otherwise `ssh` is stopped at once: the session is over either way, and
    /// waiting on a process that has nothing more to say would only delay the
    /// error.
    fn fail(&mut self, reason: String, ended: bool) -> Failure {
        self.broken = true;
        self.requests = None;
        if !ended {
            let _ = self.child.kill();
        }
        let deadline = Instant::now() + STDERR_GRACE;
        while !self.stderr_reader.is_finished() && Instant::now() < deadline {
            thread::sleep(POLL_INTERVAL);
        }
        if !self.stderr_reader.is_finished() {
            let _ = self.child.kill();
        }
        let stderr = self
            .stderr
            .lock()
            .map(|held| String::from_utf8_lossy(&held).into_owned())
            .unwrap_or_default();
        Failure::Broken {
            reason,
            stderr,
            timed_out: false,
        }
    }
}

impl Drop for SftpSession {
    /// Closing stdin is how the server is told the session is over; the kill
    /// is for an `ssh` that does not take the hint. A session must not outlive
    /// the command that opened it.
    fn drop(&mut self) {
        self.requests = None;
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// `None` for "no such file", which is an answer here rather than a failure.
fn absent_as_none<T>(result: Result<T, Failure>) -> Result<Option<T>, Failure> {
    match result {
        Ok(value) => Ok(Some(value)),
        Err(Failure::Refused(refusal)) if refusal.code == wire::status::NO_SUCH_FILE => Ok(None),
        Err(other) => Err(other),
    }
}

/// Reads until `buffer` is full or the source ends, so that only the last
/// chunk of a file is ever short.
fn fill(source: &mut dyn Read, buffer: &mut [u8]) -> std::io::Result<usize> {
    let mut filled = 0;
    while filled < buffer.len() {
        match source.read(&mut buffer[filled..]) {
            Ok(0) => break,
            Ok(read) => filled += read,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
    Ok(filled)
}

fn spawn_writer(mut stdin: std::process::ChildStdin) -> Sender<Vec<u8>> {
    let (sender, receiver) = mpsc::channel::<Vec<u8>>();
    thread::spawn(move || {
        for packet in receiver {
            if stdin
                .write_all(&packet)
                .and_then(|()| stdin.flush())
                .is_err()
            {
                return;
            }
        }
    });
    sender
}

fn spawn_reader(mut stdout: std::process::ChildStdout) -> Receiver<Result<Vec<u8>, String>> {
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        loop {
            let mut header = [0_u8; 4];
            if stdout.read_exact(&mut header).is_err() {
                let _ = sender.send(Err("the connection closed".to_string()));
                return;
            }
            let length = match wire::packet_length(header) {
                Ok(length) => length,
                Err(error) => {
                    let _ = sender.send(Err(error.to_string()));
                    return;
                }
            };
            let mut body = vec![0_u8; length];
            if stdout.read_exact(&mut body).is_err() {
                let _ = sender.send(Err(
                    "the connection closed in the middle of a reply".to_string()
                ));
                return;
            }
            if sender.send(Ok(body)).is_err() {
                return;
            }
        }
    });
    receiver
}

fn spawn_stderr(
    mut stderr: std::process::ChildStderr,
    sink: Arc<Mutex<Vec<u8>>>,
) -> JoinHandle<()> {
    thread::spawn(move || {
        let mut chunk = [0_u8; 4096];
        loop {
            match stderr.read(&mut chunk) {
                Ok(0) | Err(_) => return,
                Ok(read) => match sink.lock() {
                    Ok(mut held) => held.extend_from_slice(&chunk[..read]),
                    Err(_) => return,
                },
            }
        }
    })
}
