//! The one place in Clift that starts an external process.
//!
//! Two rules shape everything here, and both come from the specification:
//!
//! 1. Clift drives the user's own `ssh` executable. It does not link an SSH
//!    library, does not read key material and does not keep credentials of
//!    its own.
//! 2. No argument Clift generates may weaken host key verification. Clift
//!    generates exactly three `-o` options, and all three are the `Control*`
//!    settings that reuse a connection. The
//!    test below asserts the whole set by listing it: three names anyone can
//!    read is a stronger guarantee than a blacklist, which only ever catches
//!    the weakenings somebody thought of. It was "no options at all" until
//!    reuse arrived, and `ControlPersist` has no dedicated flag to pass
//!    instead.
//!
//! Remote work goes through SFTP rather than a remote shell. A remote shell
//! would mean pasting user-controlled paths into a command line the remote
//! `sh` then re-parses, which the specification forbids. Over SFTP each path
//! is its own length-prefixed protocol field, which `ssh` carries to the
//! server's SFTP subsystem untouched; see [`crate::session`].

use crate::errmap::{map_failure, map_refusal};
use crate::reuse::Reuse;
use crate::session::{Failure, SftpSession};
use clift_core::domain::RemotePath;
use clift_core::error::{CliftError, ErrorKind, Remedy, Stage};
use clift_core::ports::TransportTarget;
use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::ffi::{OsStr, OsString};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

/// How long a single `ssh` or `sftp` invocation may run before it is killed.
///
/// This is a guard against a hang, not a connection timeout. It has to leave
/// room for the things Clift deliberately does not take over: an ssh-agent
/// passphrase prompt, or a touch on a hardware key.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(120);

/// How often a running child is checked for completion. The standard library
/// has no "wait with timeout", and a dependency for one is not worth it.
const POLL_INTERVAL: Duration = Duration::from_millis(20);

/// How long a kept session may sit quiet before [`SshRunner::tend_sessions`]
/// checks that it is still alive.
const QUIET_BEFORE_CHECK: Duration = Duration::from_secs(60);

/// How long that check may take. Short, because a key press may be waiting
/// behind it.
const CHECK_LIMIT: Duration = Duration::from_secs(10);

/// What an invocation produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandOutcome {
    /// `None` when the process was terminated by a signal.
    pub code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

impl CommandOutcome {
    #[must_use]
    pub fn succeeded(&self) -> bool {
        self.code == Some(0)
    }
}

/// Runs `ssh` on behalf of the transport adapter.
#[derive(Debug, Clone)]
pub struct SshRunner {
    ssh: PathBuf,
    config_file: Option<PathBuf>,
    timeout: Duration,
    /// Connection reuse, when the caller asked for it.
    reuse: Option<Reuse>,
    /// Per host: may Clift add its own multiplexing options, or does the user
    /// already have their own? Shared between clones because it is an answer
    /// about the machine, and asking `ssh -G` once per operation would undo
    /// part of what reuse is for.
    consulted: Arc<Mutex<HashMap<String, bool>>>,
    /// One live SFTP session per host, when the caller asked for it.
    /// Shared between clones for the same reason as `consulted`: a session is a
    /// property of this run against that host, not of whichever clone of the
    /// runner happens to be holding it.
    sessions: Option<Arc<Mutex<HashMap<String, Kept>>>>,
    /// How long a kept session may go unused before it is closed. `None` keeps
    /// it for as long as the runner lives, which for a single command is the
    /// command.
    idle_limit: Option<Duration>,
    /// Per host: what it said about its cache directory. Only consulted while
    /// sessions are kept; see [`Self::remembered_cache_home`].
    cache_homes: Arc<Mutex<HashMap<String, Remembered>>>,
    /// How long a kept session may sit quiet before it is checked, and how
    /// long that check may take.
    quiet_before_check: Duration,
    check_limit: Duration,
}

/// A host's answer about its cache directory, and when it was last relied on.
#[derive(Debug)]
struct Remembered {
    cache_home: Option<RemotePath>,
    last_used: Instant,
}

/// A kept session, and when it was last put to work and last checked.
#[derive(Debug)]
struct Kept {
    session: SftpSession,
    last_used: Instant,
    last_checked: Instant,
}

impl Kept {
    fn new(session: SftpSession) -> Self {
        let now = Instant::now();
        Self {
            session,
            last_used: now,
            last_checked: now,
        }
    }
}

impl Default for SshRunner {
    fn default() -> Self {
        Self::new()
    }
}

impl SshRunner {
    /// Uses the `ssh` found on `PATH` and the user's own SSH configuration.
    #[must_use]
    pub fn new() -> Self {
        Self {
            ssh: PathBuf::from("ssh"),
            config_file: None,
            timeout: DEFAULT_TIMEOUT,
            reuse: None,
            consulted: Arc::new(Mutex::new(HashMap::new())),
            sessions: None,
            idle_limit: None,
            cache_homes: Arc::new(Mutex::new(HashMap::new())),
            quiet_before_check: QUIET_BEFORE_CHECK,
            check_limit: CHECK_LIMIT,
        }
    }

    /// Reuses one connection across invocations.
    ///
    /// Off unless asked for, so that a caller which has not thought about it
    /// gets the behaviour it had before.
    #[must_use]
    pub fn with_reuse(mut self, reuse: Reuse) -> Self {
        self.reuse = Some(reuse);
        self
    }

    /// The reuse settings, if any. Exposed so a caller can report what it did.
    #[must_use]
    pub const fn reuse(&self) -> Option<&Reuse> {
        self.reuse.as_ref()
    }

    /// Keeps one SFTP session open per host instead of starting one per
    /// operation.
    ///
    /// Off unless asked for, so a caller that has not thought about it gets
    /// one connection per operation. Nothing about the result changes: the
    /// same requests get the same answers, without a new connection and a new
    /// `sftp-server` on the far side each time. On a client that cannot
    /// multiplex, Windows among them, this is the difference between one
    /// authentication per run and one per operation.
    #[must_use]
    pub fn with_sessions(mut self) -> Self {
        self.sessions = Some(Arc::new(Mutex::new(HashMap::new())));
        self
    }

    /// Closes a kept session once it has gone unused for `limit`.
    ///
    /// For a process that lives between operations, such as the hotkey
    /// helper: one connection serves every press made within the limit, and
    /// none outlives it. It is the promise `ControlPersist` makes, kept by
    /// Clift itself, so that it also holds where the client has no
    /// `ControlPersist`.
    #[must_use]
    pub fn with_idle_limit(mut self, limit: Duration) -> Self {
        self.idle_limit = Some(limit);
        self
    }

    /// Checks a kept session once it has been quiet for `quiet`, allowing the
    /// check `limit`.
    ///
    /// The defaults suit a key press. This exists so that the integration
    /// tests can see what a check does to a real connection without sitting
    /// through a minute of quiet first.
    #[must_use]
    pub fn with_liveness_check(mut self, quiet: Duration, limit: Duration) -> Self {
        self.quiet_before_check = quiet;
        self.check_limit = limit;
        self
    }

    /// Closes kept sessions that have been idle past the limit, and checks
    /// that the others are still alive.
    ///
    /// A connection that has sat quiet for a while can have been dropped by
    /// something in between, a NAT or a proxy, without `ssh` noticing. The
    /// first request on it would then wait out the whole timeout while the
    /// user looks at an empty prompt. A cheap request every so often finds
    /// that out ahead of time, and keeps such a middlebox from dropping the
    /// connection in the first place. A session that fails the check is
    /// closed, and the next operation opens a new one.
    pub fn tend_sessions(&self) {
        let Some(sessions) = &self.sessions else {
            return;
        };
        let mut open = match sessions.lock() {
            Ok(open) => open,
            Err(poisoned) => poisoned.into_inner(),
        };
        let now = Instant::now();
        let idle_limit = self.idle_limit;
        let quiet_before_check = self.quiet_before_check;
        let check_limit = self.check_limit;
        open.retain(|_, kept| {
            if idle_limit.is_some_and(|limit| now.duration_since(kept.last_used) >= limit) {
                return false;
            }
            if !kept.session.is_usable() {
                return false;
            }
            if now.duration_since(kept.last_checked) < quiet_before_check {
                return true;
            }
            kept.session.start_operation_within(check_limit);
            let alive = kept.session.realpath(".").is_ok();
            kept.last_checked = Instant::now();
            alive
        });
    }

    /// What the host said about its cache directory, if it was asked recently
    /// enough to rely on. `None` means ask.
    ///
    /// The question is an `ssh` command of its own, not an SFTP request, so a
    /// kept session does not make it cheaper: on a client that cannot reuse
    /// connections it is a whole login on every key press. A runner that keeps
    /// sessions keeps the answer on the same terms, until it has gone unused
    /// for the idle limit. Nothing is written anywhere; the answer ends with
    /// the process at the latest.
    pub(crate) fn remembered_cache_home(
        &self,
        target: &TransportTarget,
    ) -> Option<Option<RemotePath>> {
        self.sessions.as_ref()?;
        let host = target.ssh_host();
        let mut remembered = self.cache_homes.lock().ok()?;
        let entry = remembered.get_mut(host)?;
        if self
            .idle_limit
            .is_some_and(|limit| entry.last_used.elapsed() >= limit)
        {
            remembered.remove(host);
            return None;
        }
        entry.last_used = Instant::now();
        Some(entry.cache_home.clone())
    }

    /// Records the host's answer for [`Self::remembered_cache_home`], when
    /// this runner keeps sessions.
    pub(crate) fn remember_cache_home(
        &self,
        target: &TransportTarget,
        cache_home: Option<RemotePath>,
    ) {
        if self.sessions.is_none() {
            return;
        }
        if let Ok(mut remembered) = self.cache_homes.lock() {
            remembered.insert(
                target.ssh_host().to_string(),
                Remembered {
                    cache_home,
                    last_used: Instant::now(),
                },
            );
        }
    }

    /// Whether this runner keeps SFTP sessions open. Exposed so a caller can
    /// report what it did.
    #[must_use]
    pub const fn keeps_sessions(&self) -> bool {
        self.sessions.is_some()
    }

    /// Reads SSH configuration from `path` instead of the user's own.
    ///
    /// This exists so that the integration tests can drive a throwaway
    /// container without touching, or depending on, the developer's
    /// `~/.ssh/config`. It changes which file `ssh` reads; it cannot introduce
    /// a command line option, so it is not a way around the rule above.
    #[must_use]
    pub fn with_config_file(mut self, path: impl Into<PathBuf>) -> Self {
        self.config_file = Some(path.into());
        self
    }

    #[must_use]
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    #[must_use]
    pub fn timeout(&self) -> Duration {
        self.timeout
    }

    /// The exact argument list `ssh` would be given.
    ///
    /// Exposed because "which arguments does Clift generate" is a security
    /// property, and a property that is only checked by reading the code is
    /// not checked.
    #[must_use]
    pub fn ssh_args(&self, target: &TransportTarget, command: &'static str) -> Vec<OsString> {
        let mut args = self.config_args();
        args.extend(self.reuse_args(target));
        args.push(OsString::from(target.ssh_host()));
        args.push(OsString::from(command));
        args
    }

    /// The exact argument list `ssh` is given for an SFTP session.
    ///
    /// `-s` names a subsystem instead of a command, so nothing reaches a
    /// remote shell: the server starts its own SFTP server and connects it to
    /// the channel.
    #[must_use]
    pub fn subsystem_args(&self, target: &TransportTarget) -> Vec<OsString> {
        let mut args = self.config_args();
        args.extend(self.reuse_args(target));
        args.push(OsString::from("-s"));
        args.push(OsString::from(target.ssh_host()));
        args.push(OsString::from("sftp"));
        args
    }

    /// The exact argument list for asking `ssh` what it would do with an alias.
    ///
    /// `-G` resolves the configuration and prints it; no connection is made.
    #[must_use]
    pub fn config_dump_args(&self, target: &TransportTarget) -> Vec<OsString> {
        let mut args = self.config_args();
        args.push(OsString::from("-G"));
        args.push(OsString::from(target.ssh_host()));
        args
    }

    /// The `ssh` program itself, for a caller that runs it without capturing.
    ///
    /// Exposed so that the interactive wrapper starts the same client as
    /// everything else here, including the one an integration test substitutes.
    #[must_use]
    pub fn ssh_program(&self) -> &Path {
        &self.ssh
    }

    /// The program name, for an error message.
    #[must_use]
    pub fn ssh_program_name(&self) -> String {
        self.ssh.display().to_string()
    }

    /// `-F <file>`, when a test has pointed this runner at a throwaway
    /// configuration; empty otherwise, which is the shipped case.
    #[must_use]
    pub fn config_file_args(&self) -> Vec<OsString> {
        self.config_args()
    }

    /// The multiplexing options for this host, or none.
    ///
    /// None in three cases, and all three end with an ordinary connection
    /// rather than an error: reuse was not asked for, the user already
    /// multiplexes this host, or Clift could
    /// not find out which of those it is. The last is deliberately the
    /// conservative answer -- an `ssh -G` that will not run is not a licence
    /// to override a setting that might be there.
    fn reuse_args(&self, target: &TransportTarget) -> Vec<OsString> {
        let Some(reuse) = &self.reuse else {
            return Vec::new();
        };
        if self.user_multiplexes(target) {
            return Vec::new();
        }
        reuse.options()
    }

    /// Whether the user's own configuration already multiplexes this host,
    /// asked once per host per process.
    fn user_multiplexes(&self, target: &TransportTarget) -> bool {
        let host = target.ssh_host().to_string();
        if let Ok(consulted) = self.consulted.lock()
            && let Some(answer) = consulted.get(&host)
        {
            return *answer;
        }
        // `ssh -G` resolves the configuration and prints it without connecting
        // to anything, so this costs no round trip. It is asked with the same
        // arguments as everything else except the reuse options themselves --
        // passing those here would make the client report Clift's own settings
        // back, and the answer would always be "yes, already multiplexed".
        let answer = match self.run_ssh_config_dump(target) {
            Ok(outcome) if outcome.succeeded() => {
                clift_core::context::multiplexes_already(&outcome.stdout)
            }
            _ => true,
        };
        if let Ok(mut consulted) = self.consulted.lock() {
            consulted.insert(host, answer);
        }
        answer
    }

    fn config_args(&self) -> Vec<OsString> {
        match &self.config_file {
            Some(path) => vec![OsString::from("-F"), path.clone().into_os_string()],
            None => Vec::new(),
        }
    }

    /// The local `ssh` client's version banner.
    ///
    /// Doubles as the "is OpenSSH installed" check: the banner can only be
    /// produced by a client that exists and starts.
    ///
    /// # Errors
    /// Fails when `ssh` cannot be started.
    pub fn ssh_version(&self) -> Result<String, CliftError> {
        let output = Command::new(&self.ssh)
            .arg("-V")
            .stdin(Stdio::null())
            .output()
            .map_err(|error| self.spawn_failed(&self.ssh, error))?;
        // OpenSSH prints its version on stderr and exits successfully.
        Ok(String::from_utf8_lossy(&output.stderr).trim().to_string())
    }

    /// Runs a fixed command on the remote host.
    ///
    /// `command` is `&'static str` on purpose: a `format!` result cannot be
    /// passed, so no user-controlled path can reach the remote login shell.
    /// Anything that involves a path must go through [`Self::sftp`].
    ///
    /// # Errors
    /// Fails when `ssh` cannot be started or does not finish within the
    /// timeout. A non-zero exit status is reported in the outcome, not as an
    /// error: deciding what it means is the caller's job.
    pub fn run_ssh(
        &self,
        target: &TransportTarget,
        command: &'static str,
    ) -> Result<CommandOutcome, CliftError> {
        self.run(&self.ssh, &self.ssh_args(target, command), target)
    }

    /// Asks the local `ssh` client what its configuration says about an alias.
    ///
    /// # Errors
    /// Fails when `ssh` cannot be started or does not finish within the
    /// timeout. A non-zero exit status is reported in the outcome.
    pub fn run_ssh_config_dump(
        &self,
        target: &TransportTarget,
    ) -> Result<CommandOutcome, CliftError> {
        self.run(&self.ssh, &self.config_dump_args(target), target)
    }

    /// Runs `action` in an SFTP session with the host.
    ///
    /// With sessions kept, the session opened by the first operation serves
    /// every later one. A kept session that has gone away in the meantime --
    /// `ssh` exited, or an earlier operation broke it -- is replaced before
    /// anything is sent on it, which is the only reopening there is: once a
    /// request has been written, a failure is reported, never retried. A
    /// session that breaks during `action` is dropped with the failure.
    ///
    /// # Errors
    /// Returns the session's failure unchanged; [`Self::session_error`] turns
    /// it into something a user can act on.
    pub fn sftp<T>(
        &self,
        target: &TransportTarget,
        action: impl FnOnce(&mut SftpSession) -> Result<T, Failure>,
    ) -> Result<T, Failure> {
        let Some(sessions) = &self.sessions else {
            let mut session = self.open_session(target)?;
            session.start_operation();
            return action(&mut session);
        };
        let mut open = match sessions.lock() {
            Ok(open) => open,
            // A panic elsewhere cannot leave a map of sessions half-written in
            // a way that matters: the worst case is a session that is not
            // usable, and that is checked below.
            Err(poisoned) => poisoned.into_inner(),
        };
        let host = target.ssh_host().to_string();
        let idle_limit = self.idle_limit;
        let kept = match open.entry(host.clone()) {
            Entry::Occupied(entry) => {
                let existing = entry.into_mut();
                let expired = idle_limit.is_some_and(|limit| existing.last_used.elapsed() >= limit);
                if expired || !existing.session.is_usable() {
                    *existing = Kept::new(self.open_session(target)?);
                }
                existing
            }
            Entry::Vacant(entry) => entry.insert(Kept::new(self.open_session(target)?)),
        };
        kept.session.start_operation();
        let outcome = action(&mut kept.session);
        kept.last_used = Instant::now();
        kept.last_checked = kept.last_used;
        if matches!(outcome, Err(Failure::Broken { .. })) {
            open.remove(&host);
        }
        outcome
    }

    fn open_session(&self, target: &TransportTarget) -> Result<SftpSession, Failure> {
        SftpSession::open(&self.ssh, &self.subsystem_args(target), self.timeout)
    }

    /// The error for a failed SFTP operation.
    ///
    /// `action` is what Clift was trying to do, phrased for the user. A
    /// refusal is the server's answer and keeps its words; a broken session is
    /// classified by what `ssh` printed, exactly as a failed `ssh` command is,
    /// so a rejected key or a missing subsystem reads the same either way.
    #[must_use]
    pub fn session_error(
        &self,
        target: &TransportTarget,
        stage: Stage,
        action: &str,
        failure: Failure,
    ) -> CliftError {
        match failure {
            Failure::Refused(refusal) => {
                map_refusal(target, stage, action, refusal.code, &refusal.message)
            }
            Failure::Broken {
                timed_out: true, ..
            } => self.timed_out(&self.ssh, target),
            Failure::Broken { reason, stderr, .. } => {
                let said = if stderr.trim().is_empty() {
                    reason
                } else {
                    stderr
                };
                map_failure(target, stage, action, &said)
            }
            Failure::NotStarted(error) => self.spawn_failed(&self.ssh, error),
            Failure::Local(error) => CliftError::new(
                Stage::Transfer,
                ErrorKind::Transfer,
                format!("{action}: the local file could not be read"),
            )
            .with_source(error),
        }
    }

    fn run(
        &self,
        program: &Path,
        args: &[OsString],
        target: &TransportTarget,
    ) -> Result<CommandOutcome, CliftError> {
        let mut child = Command::new(program)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|error| self.spawn_failed(program, error))?;

        // Drained on their own threads so that a child which fills one pipe
        // while the other is being read cannot deadlock against us.
        let stdout_reader = spawn_reader(child.stdout.take());
        let stderr_reader = spawn_reader(child.stderr.take());

        let status = self.wait_with_timeout(&mut child, program, target)?;

        Ok(CommandOutcome {
            code: status,
            stdout: collect(stdout_reader, "stdout")?,
            stderr: collect(stderr_reader, "stderr")?,
        })
    }

    fn wait_with_timeout(
        &self,
        child: &mut Child,
        program: &Path,
        target: &TransportTarget,
    ) -> Result<Option<i32>, CliftError> {
        let started = Instant::now();
        loop {
            match child.try_wait() {
                Ok(Some(status)) => return Ok(status.code()),
                Ok(None) => {}
                Err(error) => {
                    return Err(CliftError::new(
                        Stage::Connect,
                        ErrorKind::SshConnection,
                        format!("could not wait for {}", program.display()),
                    )
                    .with_source(error));
                }
            }
            if started.elapsed() >= self.timeout {
                let _ = child.kill();
                let _ = child.wait();
                return Err(self.timed_out(program, target));
            }
            thread::sleep(POLL_INTERVAL);
        }
    }

    fn timed_out(&self, program: &Path, target: &TransportTarget) -> CliftError {
        let name = program
            .file_name()
            .unwrap_or_else(|| OsStr::new("ssh"))
            .to_string_lossy()
            .into_owned();
        let host = target.ssh_host().to_string();
        CliftError::new(
            Stage::Connect,
            ErrorKind::SshConnection,
            format!(
                "{name} to {host} did not finish within {} seconds and was stopped",
                self.timeout.as_secs()
            ),
        )
        .with_remedy(Remedy::new(
            "Check the connection by hand:",
            format!("ssh {host}"),
        ))
    }

    fn spawn_failed(&self, program: &Path, error: std::io::Error) -> CliftError {
        let name = program
            .file_name()
            .unwrap_or_else(|| OsStr::new("ssh"))
            .to_string_lossy()
            .into_owned();
        CliftError::new(
            Stage::Connect,
            ErrorKind::SshConnection,
            format!("could not run {name}"),
        )
        .with_remedy(Remedy::new(
            format!("Clift uses the system OpenSSH client. Check that {name} is installed:"),
            format!("command -v {name}"),
        ))
        .with_source(error)
    }
}

fn spawn_reader<R: Read + Send + 'static>(
    source: Option<R>,
) -> JoinHandle<std::io::Result<Vec<u8>>> {
    thread::spawn(move || {
        let mut buffer = Vec::new();
        if let Some(mut source) = source {
            source.read_to_end(&mut buffer)?;
        }
        Ok(buffer)
    })
}

fn collect(
    handle: JoinHandle<std::io::Result<Vec<u8>>>,
    stream: &str,
) -> Result<String, CliftError> {
    match handle.join() {
        Ok(Ok(bytes)) => Ok(String::from_utf8_lossy(&bytes).into_owned()),
        Ok(Err(error)) => Err(CliftError::new(
            Stage::Connect,
            ErrorKind::SshConnection,
            format!("could not read the {stream} of the SSH client"),
        )
        .with_source(error)),
        Err(payload) => Err(CliftError::new(
            Stage::Internal,
            ErrorKind::Internal,
            format!(
                "the {stream} reader thread panicked: {}",
                panic_message(&payload)
            ),
        )),
    }
}

fn panic_message(payload: &Box<dyn std::any::Any + Send>) -> String {
    if let Some(text) = payload.downcast_ref::<&str>() {
        return (*text).to_string();
    }
    if let Some(text) = payload.downcast_ref::<String>() {
        return text.clone();
    }
    "no message".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target() -> TransportTarget {
        TransportTarget::new("core")
    }

    #[test]
    fn ssh_is_given_the_host_and_the_command_and_nothing_else() {
        let runner = SshRunner::new();
        assert_eq!(runner.ssh_args(&target(), "true"), vec!["core", "true"]);
    }

    /// Sessions are kept on every platform once asked for: SFTP spoken over
    /// `ssh` arrives as it is written everywhere, Windows included.
    #[test]
    fn sessions_are_kept_when_asked_for_and_only_then() {
        assert!(SshRunner::new().with_sessions().keeps_sessions());
        assert!(!SshRunner::new().keeps_sessions(), "off unless asked for");
    }

    #[test]
    fn sftp_is_asked_for_as_a_subsystem_not_as_a_command() {
        let runner = SshRunner::new();
        assert_eq!(runner.subsystem_args(&target()), vec!["-s", "core", "sftp"]);
    }

    /// A runner that will not consult `ssh` about the host, because the answer
    /// has been put in front of it. Keeps these tests to argument building,
    /// which is what they are about.
    fn runner_told(reuse: bool, user_multiplexes: bool) -> SshRunner {
        let runner = if reuse {
            SshRunner::new().with_reuse(
                crate::reuse::Reuse::in_directory(
                    Path::new("/run/clift"),
                    Duration::from_secs(600),
                )
                .expect("a short path"),
            )
        } else {
            SshRunner::new()
        };
        runner
            .consulted
            .lock()
            .expect("no other thread holds this")
            .insert(target().ssh_host().to_string(), user_multiplexes);
        runner
    }

    /// The specification and the specification, restated for the version of Clift that reuses
    /// connections: the *whole* set of options Clift generates is these three,
    /// and every one of them is about which socket to use rather than about
    /// what is verified. A new entry in this list is a deliberate act with a
    /// failing test attached, which is the point.
    #[test]
    fn the_only_options_clift_passes_are_the_three_that_reuse_a_connection() {
        const ALLOWED: [&str; 3] = ["ControlMaster", "ControlPath", "ControlPersist"];

        let runners = [
            runner_told(false, false),
            runner_told(true, false),
            runner_told(true, true),
            SshRunner::new().with_config_file("/somewhere/ssh_config"),
        ];
        for runner in runners {
            let mut all = runner.ssh_args(&target(), "true");
            all.extend(runner.subsystem_args(&target()));
            all.extend(runner.config_dump_args(&target()));

            let rendered: Vec<String> = all
                .iter()
                .map(|argument| argument.to_string_lossy().into_owned())
                .collect();

            for (index, text) in rendered.iter().enumerate() {
                if text != "-o" {
                    continue;
                }
                let option = rendered.get(index + 1).expect("-o without a value");
                let name = option.split_once('=').map_or(option.as_str(), |(n, _)| n);
                assert!(
                    ALLOWED.contains(&name),
                    "Clift generated the option {option:?}, which is not one of {ALLOWED:?}"
                );
            }

            let flags: Vec<&String> = rendered
                .iter()
                .filter(|text| text.starts_with('-') && *text != "-")
                .collect();
            // -F names the config file, -s asks for the SFTP subsystem, -G
            // asks ssh to print the configuration it resolved, -o carries the
            // three above. None of them changes what is verified about the
            // host.
            assert!(
                flags
                    .iter()
                    .all(|flag| ["-F", "-s", "-G", "-o"].contains(&flag.as_str())),
                "unexpected flag in {flags:?}"
            );
        }
    }

    #[test]
    fn reuse_reaches_both_clients_and_goes_before_the_host() {
        let runner = runner_told(true, false);
        let ssh: Vec<String> = runner
            .ssh_args(&target(), "true")
            .iter()
            .map(|argument| argument.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            ssh,
            vec![
                "-o",
                "ControlMaster=auto",
                "-o",
                "ControlPath=/run/clift/%C",
                "-o",
                "ControlPersist=600",
                "core",
                "true",
            ]
        );

        let sftp: Vec<String> = runner
            .subsystem_args(&target())
            .iter()
            .map(|argument| argument.to_string_lossy().into_owned())
            .collect();
        assert_eq!(sftp.last().map(String::as_str), Some("sftp"));
        assert!(
            sftp.contains(&"ControlPath=/run/clift/%C".to_string()),
            "the SFTP session shares the master too: {sftp:?}"
        );
    }

    /// A user who already multiplexes keeps their own settings. An
    /// option on the command line would beat their configuration file, so the
    /// only way to honour it is to pass nothing.
    #[test]
    fn a_host_the_user_already_multiplexes_gets_no_options_from_clift() {
        let runner = runner_told(true, true);
        for argument in runner.ssh_args(&target(), "true") {
            assert_ne!(
                argument, "-o",
                "Clift overrode the user's own ControlMaster"
            );
        }
        for argument in runner.subsystem_args(&target()) {
            assert_ne!(argument, "-o");
        }
    }

    /// The question is what the *user's* configuration says, so it must not be
    /// asked with Clift's own answer already in the arguments. This is the one
    /// place where a leaked option would not weaken anything and would still
    /// break the feature: `ssh -G` would report multiplexing that Clift itself
    /// had just put there, and Clift would then stand down in favour of it.
    #[test]
    fn the_question_about_multiplexing_is_asked_without_clifts_own_answer() {
        let runner = runner_told(true, false);
        assert_eq!(runner.config_dump_args(&target()), vec!["-G", "core"]);
    }

    #[test]
    fn a_config_file_is_passed_with_dash_f() {
        let runner = SshRunner::new().with_config_file("/tmp/x/ssh_config");
        assert_eq!(
            runner.ssh_args(&target(), "true"),
            vec!["-F", "/tmp/x/ssh_config", "core", "true"]
        );
    }
}
