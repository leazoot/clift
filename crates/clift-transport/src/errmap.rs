//! Turning OpenSSH's output into errors a user can act on.
//!
//! Two things must survive this translation. The first is attribution: a
//! missing SFTP subsystem, a rejected key and a changed host key are three
//! different problems with three different fixes, and collapsing them into
//! "connection failed" is the loss of information the specification forbids. The second is
//! OpenSSH's own text, which is attached as the error's cause so that `--debug`
//! shows it in full while the summary line stays short.
//!
//! Every pattern matched here comes from a real run; the captures live in
//! `tests/fixtures/ssh-stderr/` with a note on how each was produced. Nothing
//! is matched that has not been observed.
//!
//! No branch here suggests turning host key verification off. The specification rules it
//! out, and a changed host key is precisely the case where a convenient
//! suggestion would do the most damage.

use clift_core::error::{CliftError, ErrorKind, Remedy, Stage};
use clift_core::ports::TransportTarget;
use std::error::Error;
use std::fmt;

/// What an OpenSSH failure says about where it failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Symptom {
    /// The recorded host key and the one offered do not match.
    HostKeyChanged,
    /// The host is not in `known_hosts` at all.
    HostKeyUnknown,
    /// The server declined every key or password offered.
    AuthenticationRejected,
    /// The login worked but the server offers no SFTP subsystem.
    SftpSubsystemMissing,
    /// The connection never got as far as a login.
    Unreachable,
    /// The remote account may not write where Clift asked it to.
    RemotePermissionDenied,
    /// The remote path is not there.
    RemoteMissing,
    /// A plain SFTP failure, which is what a full disk looks like.
    TransferFailed,
    /// Nothing recognised; the text is passed through untouched.
    Unrecognised,
}

/// Classifies one OpenSSH stderr block.
///
/// Order matters twice over. A changed host key also prints the generic
/// verification failure, so it has to be tested first. And an authentication
/// refusal is `user@host: Permission denied (publickey).` while a remote write
/// refusal is `remote mkdir "...": Permission denied` -- the parenthesised list
/// of methods is what tells them apart.
#[must_use]
pub fn classify(stderr: &str) -> Symptom {
    let text = stderr.to_lowercase();
    if text.contains("remote host identification has changed") {
        return Symptom::HostKeyChanged;
    }
    if text.contains("host key verification failed") {
        return Symptom::HostKeyUnknown;
    }
    if text.contains("permission denied (") {
        return Symptom::AuthenticationRejected;
    }
    if text.contains("subsystem request failed") {
        return Symptom::SftpSubsystemMissing;
    }
    if text.contains("permission denied") {
        return Symptom::RemotePermissionDenied;
    }
    if text.contains("no such file or directory") || text.contains("not found") {
        return Symptom::RemoteMissing;
    }
    if text.contains("connection refused")
        || text.contains("connection closed")
        || text.contains("no route to host")
        || text.contains("operation timed out")
        || text.contains("connection timed out")
        || text.contains("network is unreachable")
    {
        return Symptom::Unreachable;
    }
    if text.contains(": failure") {
        return Symptom::TransferFailed;
    }
    Symptom::Unrecognised
}

impl Symptom {
    /// The exit code this symptom leads to, per the specification.
    fn kind(self, stage: Stage) -> ErrorKind {
        match self {
            Symptom::HostKeyChanged
            | Symptom::HostKeyUnknown
            | Symptom::AuthenticationRejected
            | Symptom::SftpSubsystemMissing
            | Symptom::Unreachable => ErrorKind::SshConnection,
            Symptom::RemotePermissionDenied => ErrorKind::RemoteDirectory,
            Symptom::RemoteMissing | Symptom::TransferFailed => ErrorKind::Transfer,
            // An unrecognised failure is attributed to the stage that was
            // running, which is the only thing actually known about it.
            Symptom::Unrecognised => match stage {
                Stage::Connect => ErrorKind::SshConnection,
                _ => ErrorKind::Transfer,
            },
        }
    }

    fn remedy(self, host: &str) -> Remedy {
        match self {
            Symptom::HostKeyChanged => Remedy::new(
                "Do not go further until you know why the key changed: ask whoever runs the host. \
                 Connect by hand to see what OpenSSH reports:",
                format!("ssh {host}"),
            ),
            Symptom::HostKeyUnknown => Remedy::new(
                "Connect once by hand, check the fingerprint, and let OpenSSH record it:",
                format!("ssh {host}"),
            ),
            Symptom::AuthenticationRejected => Remedy::new(
                "Check which key the server accepts:",
                format!("ssh -v {host}"),
            ),
            Symptom::SftpSubsystemMissing => Remedy::new(
                "Clift needs the server's SFTP subsystem. Check whether it is enabled:",
                format!("ssh {host} grep -i subsystem /etc/ssh/sshd_config"),
            ),
            Symptom::Unreachable => Remedy::new(
                "Check that the host is up and that the connection works at all:",
                format!("ssh {host}"),
            ),
            Symptom::RemotePermissionDenied => Remedy::new(
                "Check what the remote account is allowed to write:",
                format!("ssh {host} ls -ld ~"),
            ),
            Symptom::RemoteMissing => Remedy::new(
                "Check what is actually on the remote side:",
                format!("ssh {host} ls -la ~"),
            ),
            Symptom::TransferFailed => Remedy::new(
                "SFTP reports a plain failure, which is also what a full disk looks like. \
                 Check the free space:",
                format!("ssh {host} df -h ~"),
            ),
            Symptom::Unrecognised => Remedy::new(
                "Reproduce it by hand to see the full output:",
                format!("ssh -v {host}"),
            ),
        }
    }
}

/// OpenSSH's output, carried as the cause of a [`CliftError`].
///
/// Exists so that the summary the user reads stays one line while `--debug`
/// can still show every word the client printed.
#[derive(Debug, Clone)]
pub struct OpenSshOutput(String);

impl OpenSshOutput {
    #[must_use]
    pub fn new(stderr: impl Into<String>) -> Self {
        Self(stderr.into())
    }
}

impl fmt::Display for OpenSshOutput {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0.trim())
    }
}

impl Error for OpenSshOutput {}

/// Builds the error for a failed `ssh` or `sftp` invocation.
///
/// `action` is what Clift was trying to do, phrased for the user, for example
/// `could not create /home/dev/.cache/clift/inbox`.
#[must_use]
pub fn map_failure(
    target: &TransportTarget,
    stage: Stage,
    action: &str,
    stderr: &str,
) -> CliftError {
    let symptom = classify(stderr);
    let host = target.ssh_host();
    CliftError::new(
        stage,
        symptom.kind(stage),
        format!("{action} on {host}: {}", summarise(stderr)),
    )
    .with_remedy(symptom.remedy(host))
    .with_source(OpenSshOutput::new(stderr))
}

/// The one line worth putting in front of the user.
///
/// `sftp` echoes each batch command it runs, and a changed host key prints a
/// fourteen line banner; neither belongs in a summary, and both are still in
/// the cause chain.
/// How specific a symptom is, for choosing which line of stderr to show.
///
/// OpenSSH usually says what went wrong and then says that the connection
/// ended. Showing the last line would report the consequence and hide the
/// cause -- which is how "the wrong key" and "no SFTP subsystem" came out as
/// the same message, `Connection closed`, and left the user with nothing to act
/// on.
const fn specificity(symptom: Symptom) -> u8 {
    match symptom {
        Symptom::HostKeyChanged | Symptom::HostKeyUnknown => 6,
        Symptom::AuthenticationRejected | Symptom::SftpSubsystemMissing => 5,
        Symptom::RemotePermissionDenied => 4,
        Symptom::RemoteMissing => 3,
        Symptom::TransferFailed => 2,
        Symptom::Unreachable => 1,
        Symptom::Unrecognised => 0,
    }
}

/// The one line of OpenSSH's output worth putting in front of the user.
fn summarise(stderr: &str) -> String {
    let interesting: Vec<&str> = stderr
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .filter(|line| !line.starts_with("sftp>"))
        // The banner around a changed host key is a box of asterisks; the line
        // that names the host is the one that matters.
        .filter(|line| !line.starts_with('@'))
        .collect();

    // A changed host key produces several lines that all classify the same
    // way; the one naming the host and the known_hosts entry is the useful one.
    for line in &interesting {
        if line.to_lowercase().contains("host key for") {
            return (*line).to_string();
        }
    }

    // Otherwise the most specific thing OpenSSH said. `max_by_key` keeps the
    // last maximum, so ties resolve to the later line, which is the one closer
    // to where the operation actually stopped.
    let best = interesting
        .iter()
        .filter(|line| specificity(classify(line)) > 0)
        .max_by_key(|line| specificity(classify(line)));

    if let Some(line) = best {
        return (*line).to_string();
    }
    interesting
        .last()
        .copied()
        .unwrap_or("the OpenSSH client gave no reason")
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> String {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../tests/fixtures/ssh-stderr/"
        );
        std::fs::read_to_string(format!("{path}{name}"))
            .unwrap_or_else(|error| panic!("fixture {name} is missing: {error}"))
    }

    fn target() -> TransportTarget {
        TransportTarget::new("core")
    }

    /// Every fixture, its symptom, and the exit code the specification assigns it.
    const EXPECTED: [(&str, Symptom, u8); 9] = [
        ("auth-publickey.txt", Symptom::AuthenticationRejected, 22),
        ("host-key-unknown.txt", Symptom::HostKeyUnknown, 22),
        ("host-key-changed.txt", Symptom::HostKeyChanged, 22),
        ("connection-refused.txt", Symptom::Unreachable, 22),
        (
            "sftp-subsystem-missing.txt",
            Symptom::SftpSubsystemMissing,
            22,
        ),
        (
            "remote-permission-denied.txt",
            Symptom::RemotePermissionDenied,
            25,
        ),
        ("remote-no-such-file.txt", Symptom::RemoteMissing, 23),
        ("no-space-left.txt", Symptom::TransferFailed, 23),
        // How protocol 3 says "it is already there": the same word it uses
        // for a full disk. `ensure_dir` reads this as "look at what is
        // there" rather than as any particular cause.
        ("remote-mkdir-exists.txt", Symptom::TransferFailed, 23),
    ];

    #[test]
    fn every_captured_failure_maps_to_its_symptom_and_exit_code() {
        for (name, symptom, code) in EXPECTED {
            let stderr = fixture(name);
            assert_eq!(classify(&stderr), symptom, "{name}");
            let error = map_failure(&target(), Stage::Transfer, "could not send", &stderr);
            assert_eq!(error.exit_code().as_u8(), code, "{name}");
        }
    }

    #[test]
    fn every_mapped_failure_keeps_opensshs_output_as_its_cause() {
        for (name, _, _) in EXPECTED {
            let stderr = fixture(name);
            let error = map_failure(&target(), Stage::Connect, "could not connect", &stderr);
            let chain = error.cause_chain();
            assert!(chain.len() > 1, "{name} lost its cause");
            assert_eq!(
                chain[1],
                stderr.trim(),
                "{name}: the cause must be OpenSSH's output, unedited"
            );
        }
    }

    #[test]
    fn the_summary_is_one_line_even_when_openssh_printed_fourteen() {
        let stderr = fixture("host-key-changed.txt");
        let error = map_failure(&target(), Stage::Connect, "could not connect", &stderr);
        assert!(
            !error.message().contains('\n'),
            "the summary must be one line: {}",
            error.message()
        );
        assert!(
            error.message().contains("has changed"),
            "the summary must say what happened: {}",
            error.message()
        );
        assert!(
            error.cause_chain()[1].contains("REMOTE HOST IDENTIFICATION HAS CHANGED"),
            "the full warning must still be available under --debug"
        );
    }

    /// The specification. A changed host key is exactly where a helpful shortcut would
    /// do the most damage, so this checks every branch, not just that one.
    #[test]
    fn no_remedy_ever_proposes_weakening_verification() {
        let forbidden = [
            "stricthostkeychecking",
            "userknownhostsfile",
            "ssh-keygen -r",
            "-o ",
            "disable",
            "skip",
            "ignore",
        ];
        for (name, _, _) in EXPECTED {
            let stderr = fixture(name);
            let error = map_failure(&target(), Stage::Connect, "could not connect", &stderr);
            let remedy = error.remedy().expect("every failure offers one fix");
            let text = format!("{} {}", remedy.description(), remedy.command()).to_lowercase();
            for phrase in forbidden {
                assert!(
                    !text.contains(phrase),
                    "{name}: the remedy contains {phrase:?}: {text}"
                );
            }
        }
    }

    #[test]
    fn an_unrecognised_failure_is_attributed_to_the_stage_that_was_running() {
        let stderr = "something OpenSSH 10 says that nothing here has seen";
        assert_eq!(classify(stderr), Symptom::Unrecognised);
        assert_eq!(
            map_failure(&target(), Stage::Connect, "could not connect", stderr)
                .exit_code()
                .as_u8(),
            22
        );
        assert_eq!(
            map_failure(&target(), Stage::Transfer, "could not send", stderr)
                .exit_code()
                .as_u8(),
            23
        );
    }

    #[test]
    fn the_sftp_command_echo_never_reaches_the_summary() {
        let stderr =
            "sftp> mkdir \"/root/locked/x\"\nremote mkdir \"/root/locked/x\": Permission denied";
        let error = map_failure(
            &target(),
            Stage::Staging,
            "could not create the inbox",
            stderr,
        );
        assert!(!error.message().contains("sftp>"), "{}", error.message());
        assert!(error.message().contains("Permission denied"));
        assert_eq!(error.exit_code().as_u8(), 25);
    }
}
