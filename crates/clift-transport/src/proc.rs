//! The one place in Clift that starts an external process.
//!
//! Two rules shape everything here, and both come from the specification:
//!
//! 1. Clift drives the user's own `ssh` and `sftp` executables. It does not
//!    link an SSH library, does not read key material and does not keep
//!    credentials of its own.
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
//! `sh` then re-parses, which the specification forbids; the SFTP client instead sends
//! each path as its own protocol field. The batch language still needs
//! quoting, because the local `sftp` client parses the batch script: see
//! [`SftpBatch`].

use crate::reuse::Reuse;
use crate::session::SftpSession;
use clift_core::error::{CliftError, ErrorKind, Remedy, Stage};
use clift_core::ports::TransportTarget;
use std::collections::HashMap;
use std::ffi::{OsStr, OsString};
use std::io::{Read, Write};
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

/// Runs `ssh` and `sftp` on behalf of the transport adapter.
#[derive(Debug, Clone)]
pub struct SshRunner {
    ssh: PathBuf,
    sftp: PathBuf,
    config_file: Option<PathBuf>,
    timeout: Duration,
    /// Connection reuse, when the caller asked for it.
    reuse: Option<Reuse>,
    /// Per host: may Clift add its own multiplexing options, or does the user
    /// already have their own? Shared between clones because it is an answer
    /// about the machine, and asking `ssh -G` once per operation would undo
    /// part of what reuse is for.
    consulted: Arc<Mutex<HashMap<String, bool>>>,
    /// One live `sftp` process per host, when the caller asked for it.
    /// Shared between clones for the same reason as `consulted`: a session is a
    /// property of this run against that host, not of whichever clone of the
    /// runner happens to be holding it.
    sessions: Option<Arc<Mutex<HashMap<String, SftpSession>>>>,
}

impl Default for SshRunner {
    fn default() -> Self {
        Self::new()
    }
}

impl SshRunner {
    /// Uses the `ssh` and `sftp` found on `PATH` and the user's own SSH
    /// configuration.
    #[must_use]
    pub fn new() -> Self {
        Self {
            ssh: PathBuf::from("ssh"),
            sftp: PathBuf::from("sftp"),
            config_file: None,
            timeout: DEFAULT_TIMEOUT,
            reuse: None,
            consulted: Arc::new(Mutex::new(HashMap::new())),
            sessions: None,
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

    /// Keeps one `sftp` process open per host instead of starting one per
    /// operation.
    ///
    /// Off unless asked for, so a caller that has not thought about it gets
    /// the behaviour it had before. Nothing about the result changes: a
    /// session runs the same commands and returns the same text, it just does
    /// not pay for a new `sftp-server` on the far side each time.
    #[must_use]
    pub fn with_sessions(mut self) -> Self {
        self.sessions = Some(Arc::new(Mutex::new(HashMap::new())));
        self
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

    /// The exact argument list `sftp` would be given. The batch script itself
    /// arrives on stdin, which is why `-b -` appears here.
    #[must_use]
    pub fn sftp_args(&self, target: &TransportTarget) -> Vec<OsString> {
        let mut args = self.config_args();
        args.extend(self.reuse_args(target));
        args.push(OsString::from("-b"));
        args.push(OsString::from("-"));
        args.push(OsString::from(target.ssh_host()));
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

    /// Confirms the local `sftp` client exists and starts.
    ///
    /// `sftp` has no version flag; being able to run it at all is the whole
    /// answer, so its usage message and non-zero status are both expected.
    ///
    /// # Errors
    /// Fails when `sftp` cannot be started.
    pub fn sftp_present(&self) -> Result<(), CliftError> {
        Command::new(&self.sftp)
            .arg("-h")
            .stdin(Stdio::null())
            .output()
            .map_err(|error| self.spawn_failed(&self.sftp, error))?;
        Ok(())
    }

    /// Runs a fixed command on the remote host.
    ///
    /// `command` is `&'static str` on purpose: a `format!` result cannot be
    /// passed, so no user-controlled path can reach the remote login shell.
    /// Anything that involves a path must go through [`Self::run_sftp`].
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
        self.run(
            &self.ssh,
            &self.ssh_args(target, command),
            None,
            target,
            false,
        )
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
        self.run(
            &self.ssh,
            &self.config_dump_args(target),
            None,
            target,
            false,
        )
    }

    /// Runs a batch of SFTP commands against the remote host.
    ///
    /// # Errors
    /// Fails when `sftp` cannot be started or does not finish within the
    /// timeout.
    pub fn run_sftp(
        &self,
        target: &TransportTarget,
        batch: &SftpBatch,
    ) -> Result<CommandOutcome, CliftError> {
        if let Some(outcome) = self.run_sftp_in_session(target, batch) {
            return outcome;
        }
        self.run(
            &self.sftp,
            &self.sftp_args(target),
            Some(batch.render()),
            target,
            true,
        )
    }

    /// Runs the batch in a live session, or reports that there was none to run
    /// it in.
    ///
    /// `None` means "no session was available" and asks the caller to do it the
    /// one-shot way. It is returned only when nothing has reached the server,
    /// so it can never turn into a silent retry of a batch that had already
    /// begun -- mid-transfer retries are forbidden, and a `rename`
    /// sent twice is exactly the kind of thing that forbids them.
    fn run_sftp_in_session(
        &self,
        target: &TransportTarget,
        batch: &SftpBatch,
    ) -> Option<Result<CommandOutcome, CliftError>> {
        let sessions = self.sessions.as_ref()?;
        let mut open = sessions.lock().ok()?;
        let host = target.ssh_host().to_string();

        // Two attempts, and only ever for a session that carried nothing: the
        // first covers a session the server has since closed, the second is the
        // one that reports the trouble.
        for attempt in 0..2 {
            if !open.contains_key(&host) {
                let started = SftpSession::open(&self.sftp, &self.sftp_args(target), true).ok()?;
                open.insert(host.clone(), started);
            }
            let session = open.get_mut(&host)?;
            match session.run(batch, self.timeout) {
                Ok(outcome) => {
                    return Some(Ok(CommandOutcome {
                        code: Some(i32::from(outcome.failed)),
                        stdout: outcome.stdout,
                        stderr: outcome.stderr,
                    }));
                }
                Err(error) => {
                    open.remove(&host);
                    if error.started {
                        return Some(Err(CliftError::new(
                            Stage::Transfer,
                            ErrorKind::Transfer,
                            format!("{} while talking to {host}", error.message),
                        )));
                    }
                    if attempt == 1 {
                        return None;
                    }
                }
            }
        }
        None
    }

    fn run(
        &self,
        program: &Path,
        args: &[OsString],
        stdin_data: Option<String>,
        target: &TransportTarget,
        timestamps_in_utc: bool,
    ) -> Result<CommandOutcome, CliftError> {
        let mut command = Command::new(program);
        command
            .args(args)
            .stdin(if stdin_data.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if timestamps_in_utc {
            // `sftp` renders modification times with the *client's* local
            // strftime, and the standard library cannot tell us what offset
            // that was. Pinning the child to UTC makes the rendered time
            // unambiguous, which is the only way to read it back as an absolute
            // instant without adding a timezone dependency. It affects nothing
            // else: the value is rendered locally and never reaches the server.
            command.env("TZ", "UTC0");
        }
        let mut child = command
            .spawn()
            .map_err(|error| self.spawn_failed(program, error))?;

        // The readers are drained on their own threads so that a child which
        // fills its output pipe while we are still writing its input cannot
        // deadlock against us.
        let stdout_reader = spawn_reader(child.stdout.take());
        let stderr_reader = spawn_reader(child.stderr.take());

        if let Some(data) = stdin_data {
            let write_result = match child.stdin.take() {
                Some(mut stdin) => stdin.write_all(data.as_bytes()),
                None => Ok(()),
            };
            if let Err(error) = write_result {
                let _ = child.kill();
                let _ = child.wait();
                return Err(CliftError::new(
                    Stage::Connect,
                    ErrorKind::SshConnection,
                    format!("could not send the SFTP batch to {}", target.ssh_host()),
                )
                .with_source(error));
            }
        }

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

/// A script for `sftp -b -`.
///
/// The remote side never sees this text: `sftp` parses it locally and sends
/// each path as a protocol field. What it does mean is that the *local* client
/// tokenises the script, so every operand has to be quoted for that
/// tokeniser: including against glob expansion, which `sftp` applies to
/// unquoted operands of `rm`, `ls` and `get`.
#[derive(Debug, Clone, Default)]
pub struct SftpBatch {
    lines: Vec<String>,
}

impl SftpBatch {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Appends one command.
    ///
    /// The verb is `&'static str` for the same reason as in
    /// [`SshRunner::run_ssh`]: it must never be assembled from user input.
    ///
    /// # Errors
    /// Fails when an operand contains a character the batch language cannot
    /// carry, which in practice means a control character such as a newline.
    pub fn push(&mut self, verb: &'static str, operands: &[&str]) -> Result<(), CliftError> {
        let mut line = String::from(verb);
        for operand in operands {
            line.push(' ');
            line.push_str(&quote(operand)?);
        }
        self.lines.push(line);
        Ok(())
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }

    /// The commands, one at a time.
    ///
    /// A live session sends them individually so that it can stop at the first
    /// failure the way the one-shot client stops on a batch script; see
    /// [`crate::session`].
    #[must_use]
    pub fn commands(&self) -> &[String] {
        &self.lines
    }

    /// The script as `sftp` will read it.
    #[must_use]
    pub fn render(&self) -> String {
        let mut script = String::new();
        for line in &self.lines {
            script.push_str(line);
            script.push('\n');
        }
        script
    }
}

/// Quotes one operand for the `sftp` batch tokeniser.
///
/// Inside double quotes `sftp` treats only `\` and `"` as special and performs
/// no glob expansion, so wrapping and escaping those two characters is enough.
///
/// # Errors
/// Fails on a control character: a newline would end the command and a NUL
/// cannot be transported at all. Remote names are sanitised before they reach
/// here, so this is a defence in depth rather than the primary check.
pub fn quote(operand: &str) -> Result<String, CliftError> {
    if let Some(offending) = operand.chars().find(|character| character.is_control()) {
        return Err(CliftError::new(
            Stage::Transfer,
            ErrorKind::Transfer,
            format!(
                "a remote path contains the control character U+{:04X}, which SFTP batch mode \
                 cannot carry",
                offending as u32
            ),
        ));
    }

    let mut quoted = String::with_capacity(operand.len() + 2);
    quoted.push('"');
    for character in operand.chars() {
        if character == '"' || character == '\\' {
            quoted.push('\\');
        }
        quoted.push(character);
    }
    quoted.push('"');
    Ok(quoted)
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

    #[test]
    fn sftp_reads_its_batch_from_stdin() {
        let runner = SshRunner::new();
        assert_eq!(runner.sftp_args(&target()), vec!["-b", "-", "core"]);
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
            all.extend(runner.sftp_args(&target()));
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
            // -F names the config file, -b feeds sftp its batch on stdin, -G
            // asks ssh to print the configuration it resolved, -o carries the
            // three above. None of them changes what is verified about the
            // host.
            assert!(
                flags
                    .iter()
                    .all(|flag| ["-F", "-b", "-G", "-o"].contains(&flag.as_str())),
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
            .sftp_args(&target())
            .iter()
            .map(|argument| argument.to_string_lossy().into_owned())
            .collect();
        assert_eq!(sftp.last().map(String::as_str), Some("core"));
        assert!(
            sftp.contains(&"ControlPath=/run/clift/%C".to_string()),
            "sftp shares the master too, or eight of the nine sessions are unaffected: {sftp:?}"
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
        for argument in runner.sftp_args(&target()) {
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

    #[test]
    fn quoting_wraps_and_escapes_only_what_the_tokeniser_needs() {
        assert_eq!(quote("plain").unwrap(), "\"plain\"");
        assert_eq!(quote("a b").unwrap(), "\"a b\"");
        assert_eq!(quote("截图 2.png").unwrap(), "\"截图 2.png\"");
        assert_eq!(quote("it's").unwrap(), "\"it's\"");
        assert_eq!(quote("say \"hi\"").unwrap(), "\"say \\\"hi\\\"\"");
        assert_eq!(quote("back\\slash").unwrap(), "\"back\\\\slash\"");
        // Glob metacharacters need no escape of their own: sftp does not
        // expand them inside quotes.
        assert_eq!(quote("star*x?[a]").unwrap(), "\"star*x?[a]\"");
    }

    #[test]
    fn quoting_refuses_control_characters() {
        for input in ["line\nbreak", "tab\there", "nul\0byte"] {
            let error = quote(input).unwrap_err();
            assert_eq!(error.exit_code().as_u8(), 23, "{input:?}");
            assert!(error.message().contains("control character"), "{error}");
        }
    }

    #[test]
    fn a_batch_renders_one_command_per_line() {
        let mut batch = SftpBatch::new();
        assert!(batch.is_empty());
        batch.push("mkdir", &["/home/dev/a b"]).unwrap();
        batch.push("chmod", &["700", "/home/dev/a b"]).unwrap();
        batch.push("quit", &[]).unwrap();
        assert_eq!(
            batch.render(),
            "mkdir \"/home/dev/a b\"\nchmod \"700\" \"/home/dev/a b\"\nquit\n"
        );
    }

    #[test]
    fn a_control_character_stops_the_whole_batch_rather_than_being_stripped() {
        let mut batch = SftpBatch::new();
        assert!(batch.push("mkdir", &["/home/dev/a\nrm -rf /"]).is_err());
        assert!(
            batch.is_empty(),
            "a rejected command must not be half-written into the batch"
        );
    }
}
