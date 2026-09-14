//! One `sftp` process, kept open across operations.
//!
//! Connection reuse ([`crate::reuse`], the specification) removed the cost of
//! authenticating once per operation. It did not remove the cost of *starting*
//! one: every `sftp` invocation asks the server for a fresh `sftp-server`
//! subsystem, and on the reference host that costs seconds even over an
//! established master. A single `clift send` runs eleven of them.
//!
//! Measured against that host: a command sent into a session that is already
//! open costs 0.2 ms, while opening the session costs 4.15 s. The session
//! count was therefore the whole remaining cost, which is what this module
//! removes.
//!
//! # How one process runs many batches
//!
//! `sftp -b -` reads commands from stdin and echoes each one back to stdout as
//! `sftp> <command>` before running it. That echo is the frame marker: it is
//! Clift's own text coming back, so the end of one command's output can be
//! recognised rather than guessed at from timing.
//!
//! Two details make it work.
//!
//! **Every command is sent with a leading `-`**, which tells `sftp` to carry on
//! after a failure instead of ending the session. A missing path is a normal
//! answer here -- [`crate::probe::OpenSshTransport::stat`] is implemented by
//! listing a parent that may not exist yet -- and without the prefix the first
//! such answer would tear down the very session that is meant to be shared.
//! Abort-on-first-failure is not lost, it moves here: [`SftpSession::run`]
//! stops sending a batch at the first command that writes to stderr, which is
//! where the one-shot client would have stopped too. That equivalence matters
//! beyond tidiness -- `ensure_dir` sends `mkdir` and `chmod` as a pair, and a
//! `chmod` that ran after its `mkdir` had failed would be Clift changing the
//! permissions of a directory somebody else created.
//!
//! **The fence is an invalid command carrying a random token.** `sftp` rejects
//! it locally, so it costs no round trip, and rejecting it produces both halves
//! of the frame at once: the token echoed on stdout, which can be matched
//! exactly, and `Invalid command.` on stderr, which closes the error half.
//!
//! The token is random rather than a fixed string on purpose. Standard output
//! carries remote file names, and a name containing a newline could otherwise
//! forge a frame marker -- the remote account is not a trusted source of text.
//!
//! # Not on Windows
//!
//! The Windows build of OpenSSH's `sftp` writes to a piped stdout only when it
//! exits. Sent one command at a time with stdin left open, it answered each on
//! stderr at once and printed nothing on stdout until stdin was closed. The
//! echo this module frames on never arrives while such a session is alive, so
//! `SshRunner::with_sessions` leaves sessions off there.

use crate::proc::SftpBatch;
use std::ffi::OsString;
use std::io::{Read, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

/// Bytes of randomness in the frame token. The token only has to be
/// unguessable by whoever can write file names on the remote host.
const TOKEN_BYTES: usize = 8;

/// How often the streams are checked while a command is in flight.
const POLL_INTERVAL: Duration = Duration::from_millis(2);

/// How long a stopped client's stdout may take to close. It closes as soon as
/// the process is gone; the limit is for anything else still holding the pipe,
/// and running out of it counts the command as sent.
const END_OF_OUTPUT_WAIT: Duration = Duration::from_secs(2);

/// What `sftp` printed for one batch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionOutcome {
    pub stdout: String,
    pub stderr: String,
    /// Whether a command in the batch failed, in which case the rest were not
    /// sent -- the same thing the one-shot client does with a batch script.
    pub failed: bool,
}

/// Why a session could not carry a batch.
///
/// The distinction is the whole point of the type: `started` says whether any
/// command may have reached the server, which is true from the moment `sftp`
/// has read one. Nothing may be retried once it has, because retrying a partly
/// executed batch is exactly the automatic mid-transfer retry that the
/// specification forbids.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionError {
    pub started: bool,
    pub message: String,
}

impl SessionError {
    fn before_anything_ran(message: impl Into<String>) -> Self {
        Self {
            started: false,
            message: message.into(),
        }
    }

    fn midway(message: impl Into<String>) -> Self {
        Self {
            started: true,
            message: message.into(),
        }
    }
}

/// A live `sftp -b -` process.
#[derive(Debug)]
pub struct SftpSession {
    child: Child,
    stdin: Option<ChildStdin>,
    stdout: Stream,
    stderr: Stream,
    /// The unfinished tail of each stream, kept between commands because a
    /// frame marker can arrive split across two reads.
    pending_out: Vec<u8>,
    pending_err: Vec<u8>,
    fence: String,
}

impl SftpSession {
    /// Starts `program` with `args` and waits for nothing: the first command
    /// sent is what pays for the connection.
    ///
    /// # Errors
    /// Fails when the process cannot be started or its pipes cannot be taken.
    pub fn open(
        program: &Path,
        args: &[OsString],
        timestamps_in_utc: bool,
    ) -> Result<Self, String> {
        let mut command = Command::new(program);
        command
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if timestamps_in_utc {
            // Same reason as the one-shot path: `sftp` renders modification
            // times with the client's own strftime, and only a pinned zone
            // makes them readable back as an absolute instant.
            command.env("TZ", "UTC0");
        }
        let mut child = command
            .spawn()
            .map_err(|error| format!("could not start {}: {error}", program.display()))?;

        let (Some(stdin), Some(stdout), Some(stderr)) =
            (child.stdin.take(), child.stdout.take(), child.stderr.take())
        else {
            let _ = child.kill();
            let _ = child.wait();
            return Err("the sftp client did not provide the expected pipes".to_string());
        };

        Ok(Self {
            child,
            stdin: Some(stdin),
            stdout: Stream::draining(stdout),
            stderr: Stream::draining(stderr),
            pending_out: Vec::new(),
            pending_err: Vec::new(),
            fence: fence_token(),
        })
    }

    /// Runs every command in `batch`, stopping at the first failure.
    ///
    /// # Errors
    /// Fails when the session can no longer be used. Whether anything ran
    /// first is carried on the error, because it decides whether the caller
    /// may start over.
    pub fn run(
        &mut self,
        batch: &SftpBatch,
        timeout: Duration,
    ) -> Result<SessionOutcome, SessionError> {
        let mut stdout = String::new();
        let mut started = false;

        for command in batch.commands() {
            let frame = self.run_one(command, timeout, started)?;
            started = true;
            stdout.push_str(&frame.stdout);
            if !frame.stderr.is_empty() {
                return Ok(SessionOutcome {
                    stdout,
                    stderr: frame.stderr,
                    failed: true,
                });
            }
        }

        Ok(SessionOutcome {
            stdout,
            stderr: String::new(),
            failed: false,
        })
    }

    /// Sends one command and returns everything it printed.
    fn run_one(
        &mut self,
        command: &str,
        timeout: Duration,
        started: bool,
    ) -> Result<Frame, SessionError> {
        // The leading `-` on both lines: see the module documentation. It is
        // what keeps an expected failure from ending the session.
        let script = format!("-{command}\n-{}\n", self.fence);
        let Some(stdin) = self.stdin.as_mut() else {
            let message = "the sftp session has already been closed";
            return Err(if started {
                SessionError::midway(message)
            } else {
                SessionError::before_anything_ran(message)
            });
        };
        let written = stdin
            .write_all(script.as_bytes())
            .and_then(|()| stdin.flush());
        if let Err(error) = written {
            return Err(self.abandon(
                started,
                format!("could not write to the sftp session: {error}"),
            ));
        }

        let deadline = Instant::now() + timeout;
        loop {
            self.pending_out.extend_from_slice(&self.stdout.take());
            self.pending_err.extend_from_slice(&self.stderr.take());

            if let Some(frame) =
                cut_frame(&mut self.pending_out, &mut self.pending_err, &self.fence)
            {
                return Ok(frame);
            }

            if let Ok(Some(status)) = self.child.try_wait() {
                // Drain whatever the pipes still hold before giving up: the
                // reason the client stopped is usually in there.
                thread::sleep(POLL_INTERVAL);
                self.pending_err.extend_from_slice(&self.stderr.take());
                let said = String::from_utf8_lossy(&self.pending_err)
                    .trim()
                    .to_string();
                return Err(self.abandon(
                    started,
                    format!(
                        "the sftp session ended ({status}){}",
                        if said.is_empty() {
                            String::new()
                        } else {
                            format!(": {said}")
                        }
                    ),
                ));
            }
            if Instant::now() >= deadline {
                return Err(self.abandon(
                    started,
                    format!(
                        "the sftp session did not answer within {} seconds",
                        timeout.as_secs()
                    ),
                ));
            }
            thread::sleep(POLL_INTERVAL);
        }
    }

    /// Ends a session whose command could not be seen through, and decides
    /// whether that command may have run.
    ///
    /// Having been written is not the test: a command written into the pipe
    /// may still be waiting there while the client connects. Having been read
    /// is. `sftp` echoes each command on stdout before it performs it, and its
    /// stdout is line buffered, so a command that got as far as the server was
    /// echoed first. With the client stopped and its stdout closed, an empty
    /// stdout therefore means the command was never taken. Anything else
    /// counts as sent, including a pipe that would not close: sending a
    /// command twice is what this exists to prevent, and an error that a
    /// second attempt might have avoided is the cheaper mistake.
    fn abandon(&mut self, started: bool, message: String) -> SessionError {
        self.stdin = None;
        let _ = self.child.kill();
        let _ = self.child.wait();
        if started {
            return SessionError::midway(message);
        }
        let closed = self.stdout.wait_for_end(END_OF_OUTPUT_WAIT);
        self.pending_out.extend_from_slice(&self.stdout.take());
        if closed && self.pending_out.is_empty() {
            SessionError::before_anything_ran(message)
        } else {
            SessionError::midway(message)
        }
    }
}

impl Drop for SftpSession {
    /// Closing stdin is how `sftp` is asked to exit; the kill is for a client
    /// that ignores it. A session must not outlive the command that opened it:
    /// The specification promises no process of Clift's is left running.
    fn drop(&mut self) {
        self.stdin = None;
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// What one command printed.
struct Frame {
    stdout: String,
    stderr: String,
}

/// What `sftp` says on stderr when it is handed a command it does not know.
///
/// This is produced entirely locally, by the client's own parser, which is why
/// the fence costs no round trip and cannot be influenced by the remote host.
const INVALID_COMMAND: &[u8] = b"Invalid command.";

/// Takes one command's output off the front of both streams, once the fence
/// has come back on each.
///
/// `None` while either half is incomplete, and then neither stream is touched:
/// the pipes are drained separately, so one half routinely arrives first.
/// Both markers are matched up to the end of their line rather than including
/// a line ending, because the Windows build of OpenSSH's sftp ends its lines
/// with `\r\n`.
fn cut_frame(stdout: &mut Vec<u8>, stderr: &mut Vec<u8>, fence: &str) -> Option<Frame> {
    let echo = format!("sftp> -{fence}");
    if !has_line(stdout, echo.as_bytes()) || !has_line(stderr, INVALID_COMMAND) {
        return None;
    }
    let out = cut_line(stdout, echo.as_bytes())?;
    let err = cut_line(stderr, INVALID_COMMAND)?;
    Some(Frame {
        stdout: String::from_utf8_lossy(&out).into_owned(),
        stderr: String::from_utf8_lossy(&err).into_owned(),
    })
}

/// Whether `marker` and the end of the line it sits on have both arrived.
fn has_line(buffer: &[u8], marker: &[u8]) -> bool {
    find(buffer, marker).is_some_and(|at| buffer[at..].contains(&b'\n'))
}

/// As [`cut`], but consumes the whole line the marker sits on.
///
/// The marker is followed by a line ending whose bytes differ between
/// platforms and versions, so the rule is "up to the next newline" rather than
/// a fixed suffix. `None` until that newline has arrived, so that a marker
/// split across two reads is not mistaken for a complete frame.
fn cut_line(buffer: &mut Vec<u8>, marker: &[u8]) -> Option<Vec<u8>> {
    let at = find(buffer, marker)?;
    let newline = buffer[at..].iter().position(|byte| *byte == b'\n')? + at;
    let rest = buffer.split_off(newline + 1);
    let mut content = std::mem::replace(buffer, rest);
    content.truncate(at);
    Some(content)
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    (0..=haystack.len() - needle.len())
        .find(|start| &haystack[*start..start + needle.len()] == needle)
}

/// A token no remote file name can be expected to contain.
///
/// A failure of the system random source is not fatal here and must not be:
/// this value guards a text frame, not a key. It falls back to a constant, and
/// the session then behaves exactly as it would have with a fixed marker --
/// which is to say correctly, unless somebody on the remote host has planted a
/// file name containing a newline and this exact string.
fn fence_token() -> String {
    let mut bytes = [0_u8; TOKEN_BYTES];
    if getrandom::fill(&mut bytes).is_err() {
        return "clift-fence".to_string();
    }
    let mut token = String::with_capacity(2 * TOKEN_BYTES + 12);
    token.push_str("clift-fence-");
    for byte in bytes {
        token.push_str(&format!("{byte:02x}"));
    }
    token
}

/// A pipe drained on its own thread, so a child that fills its output buffer
/// while Clift is writing to its input cannot deadlock against it.
#[derive(Debug)]
struct Stream {
    buffer: Arc<Mutex<Vec<u8>>>,
    reader: thread::JoinHandle<()>,
}

impl Stream {
    fn draining<R: Read + Send + 'static>(mut source: R) -> Self {
        let buffer = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&buffer);
        let reader = thread::spawn(move || {
            let mut chunk = [0_u8; 4096];
            loop {
                match source.read(&mut chunk) {
                    Ok(0) | Err(_) => return,
                    Ok(read) => match sink.lock() {
                        Ok(mut held) => held.extend_from_slice(&chunk[..read]),
                        Err(_) => return,
                    },
                }
            }
        });
        Self { buffer, reader }
    }

    /// Waits up to `limit` for the pipe to close, and says whether it did.
    fn wait_for_end(&self, limit: Duration) -> bool {
        let deadline = Instant::now() + limit;
        while !self.reader.is_finished() {
            if Instant::now() >= deadline {
                return false;
            }
            thread::sleep(POLL_INTERVAL);
        }
        true
    }

    /// Everything read since the last call.
    fn take(&self) -> Vec<u8> {
        match self.buffer.lock() {
            Ok(mut held) => std::mem::take(&mut *held),
            // A panicked reader thread cannot corrupt a byte buffer, and
            // treating it as "nothing new" lets the caller's timeout report the
            // situation rather than this returning a second panic.
            Err(poisoned) => std::mem::take(&mut *poisoned.into_inner()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_frame_ends_at_the_marker_and_leaves_the_rest() {
        let mut stdout =
            b"sftp> pwd\nRemote working directory: /home/dev\nsftp> -tok\nnext".to_vec();
        let mut stderr = b"Invalid command.\nlater".to_vec();
        let frame = cut_frame(&mut stdout, &mut stderr, "tok").unwrap();
        assert_eq!(
            frame.stdout,
            "sftp> pwd\nRemote working directory: /home/dev\n"
        );
        assert_eq!(
            String::from_utf8_lossy(&stdout),
            "next",
            "what arrived after the fence belongs to the next command"
        );
        assert_eq!(String::from_utf8_lossy(&stderr), "later");
    }

    #[test]
    fn a_marker_that_has_not_arrived_yet_is_not_a_frame() {
        let mut stdout = b"sftp> pwd\nsftp> -to".to_vec();
        let mut stderr = b"Invalid command.\n".to_vec();
        assert!(
            cut_frame(&mut stdout, &mut stderr, "tok").is_none(),
            "half a marker must not end a frame"
        );
        assert_eq!(
            (stdout.len(), stderr.len()),
            (19, 17),
            "and both buffers must be left intact for the next read"
        );
    }

    /// The error half is only complete once its line ending has arrived, which
    /// is what stops a marker split across two reads from ending a frame early.
    #[test]
    fn the_error_half_of_a_frame_consumes_the_whole_marker_line() {
        let mut buffer = b"Can't ls: \"/x\" not found\r\nInvalid command.".to_vec();
        assert_eq!(cut_line(&mut buffer, INVALID_COMMAND), None);

        buffer.extend_from_slice(b"\r\nCan't ls: \"/y\" not found\r\n");
        let content = cut_line(&mut buffer, INVALID_COMMAND).unwrap();
        assert_eq!(
            String::from_utf8_lossy(&content),
            "Can't ls: \"/x\" not found\r\n",
            "only what the command itself said"
        );
        assert_eq!(
            String::from_utf8_lossy(&buffer),
            "Can't ls: \"/y\" not found\r\n"
        );
    }

    /// The Windows build of OpenSSH's sftp ends every line with `\r\n`, the
    /// echoed fence included.
    #[test]
    fn a_frame_ends_at_the_fence_whatever_the_line_ending() {
        for newline in ["\n", "\r\n"] {
            let mut stdout = format!(
                "sftp> -pwd{newline}Remote working directory: /root{newline}sftp> -tok{newline}next"
            )
            .into_bytes();
            let mut stderr = format!("Invalid command.{newline}").into_bytes();
            let frame = cut_frame(&mut stdout, &mut stderr, "tok")
                .unwrap_or_else(|| panic!("no frame with {newline:?} line endings"));
            assert!(frame.stdout.contains("Remote working directory: /root"));
            assert_eq!(frame.stderr, "");
            assert_eq!(
                String::from_utf8_lossy(&stdout),
                "next",
                "what follows the fence belongs to the next command"
            );
            assert!(stderr.is_empty());
        }
    }

    /// The two pipes are drained separately, so the fence can come back on
    /// one before the other. What has already arrived must survive the wait.
    #[test]
    fn a_frame_waits_for_both_halves_without_losing_either() {
        let mut stdout = b"Remote working directory: /root\nsftp> -tok\n".to_vec();
        let mut stderr = Vec::new();
        assert!(cut_frame(&mut stdout, &mut stderr, "tok").is_none());

        stderr.extend_from_slice(b"Invalid command.\n");
        let frame = cut_frame(&mut stdout, &mut stderr, "tok")
            .unwrap_or_else(|| panic!("both halves have arrived"));
        assert_eq!(frame.stdout, "Remote working directory: /root\n");
    }

    #[cfg(unix)]
    fn a_rename() -> SftpBatch {
        let mut batch = SftpBatch::new();
        batch
            .push("rename", &["/inbox/.a.part", "/inbox/a.png"])
            .unwrap();
        batch
    }

    /// `sftp` echoes a command before it runs it, so a client that echoed the
    /// command may already have sent it to the server. `cat` stands in for
    /// that client: it takes the command and echoes it, but never produces
    /// the fence's `Invalid command.`, so the command runs into the timeout.
    /// Starting over from there would send the rename a second time.
    #[cfg(unix)]
    #[test]
    fn a_command_the_client_took_counts_as_sent_even_when_it_never_answers() {
        let mut session = SftpSession::open(Path::new("cat"), &[], false).unwrap();
        let error = session
            .run(&a_rename(), Duration::from_millis(300))
            .unwrap_err();
        assert!(
            error.started,
            "a command the client had already taken was reported as never sent: {error:?}"
        );
    }

    /// The other side of the same line: a client that exits without having
    /// read its input, the way `sftp` does when authentication fails, has run
    /// nothing, and the caller may still start over.
    #[cfg(unix)]
    #[test]
    fn a_command_the_client_never_took_may_be_sent_again() {
        let mut session =
            SftpSession::open(Path::new("sleep"), &[OsString::from("30")], false).unwrap();
        let error = session
            .run(&a_rename(), Duration::from_millis(300))
            .unwrap_err();
        assert!(
            !error.started,
            "a command nothing had read was reported as sent: {error:?}"
        );
    }

    /// The token guards against a remote file name forging a frame marker, so
    /// two sessions must not share one.
    #[test]
    fn every_session_gets_its_own_token() {
        let first = fence_token();
        let second = fence_token();
        assert_ne!(first, second);
        assert!(first.starts_with("clift-fence-"));
        assert_eq!(first.len(), "clift-fence-".len() + 2 * TOKEN_BYTES);
    }
}
