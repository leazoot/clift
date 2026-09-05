//! The single error type used across Clift.
//!
//! Every failure carries three things the user needs: the stage it happened in,
//! the underlying cause, and one command that is likely to fix it. The specification
//! requires error text to name the stage before offering a fix, so the stage is
//! part of the type rather than something each call site remembers to mention.

use std::error::Error;
use std::fmt;

/// The pipeline stage a failure belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Stage {
    Clipboard,
    TargetResolution,
    Connect,
    Transfer,
    Staging,
    Config,
    Integration,
    /// Universal Mode: sealing an attachment, talking to the relay, redeeming a
    /// token. Kept apart from `Transfer`, which means SFTP over the user's own
    /// SSH -- the two have different failure modes and different remedies, and
    /// a user reading an error needs to know which of the two paths they were
    /// on.
    Relay,
    /// Putting the paste in front of the user: the clipboard replacement of
    /// `--copy`, or the synthesised keystroke of `--inject`.
    Injection,
    Internal,
}

impl Stage {
    /// Every stage in declaration order. Tests iterate this instead of
    /// re-listing the variants, so a new stage cannot be silently untested.
    pub const ALL: [Stage; 10] = [
        Stage::Clipboard,
        Stage::TargetResolution,
        Stage::Connect,
        Stage::Transfer,
        Stage::Staging,
        Stage::Config,
        Stage::Integration,
        Stage::Relay,
        Stage::Injection,
        Stage::Internal,
    ];

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Stage::Clipboard => "clipboard",
            Stage::TargetResolution => "target resolution",
            Stage::Connect => "connect",
            Stage::Transfer => "transfer",
            Stage::Staging => "staging",
            Stage::Config => "config",
            Stage::Integration => "integration",
            Stage::Relay => "relay",
            Stage::Injection => "injection",
            Stage::Internal => "internal",
        }
    }
}

impl fmt::Display for Stage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// What went wrong, expressed in the same terms as the exit code contract.
///
/// The mapping to exit codes lives in [`crate::exit`] and is the only place in
/// the workspace allowed to define it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ErrorKind {
    /// Nothing to send. The terminal integration turns this into a native
    /// paste, so it is a normal outcome rather than a malfunction.
    NoAttachment,
    Config,
    AmbiguousTarget,
    SshConnection,
    Transfer,
    ClipboardRead,
    RemoteDirectory,
    LimitExceeded,
    /// The token cannot be redeemed: it is malformed, it names a version this
    /// build does not implement, or the object behind it is gone -- expired,
    /// already fetched, or never there.
    ///
    /// One kind for all of those on purpose. They are distinguished in the
    /// message, where a person can read them, and not in the exit code, where
    /// a script would be tempted to branch on "already fetched" and retry.
    TokenUnusable,
    /// The relay could not be reached, or answered in a way that means it is
    /// not working. Never used for "the object is not there", which is
    /// [`ErrorKind::TokenUnusable`].
    RelayUnavailable,
    /// Bytes arrived and are not what they claim to be: the authentication tag
    /// does not verify, or the frame inside does not decode.
    IntegrityFailure,
    Internal,
}

impl ErrorKind {
    pub const ALL: [ErrorKind; 12] = [
        ErrorKind::NoAttachment,
        ErrorKind::Config,
        ErrorKind::AmbiguousTarget,
        ErrorKind::SshConnection,
        ErrorKind::Transfer,
        ErrorKind::ClipboardRead,
        ErrorKind::RemoteDirectory,
        ErrorKind::LimitExceeded,
        ErrorKind::TokenUnusable,
        ErrorKind::RelayUnavailable,
        ErrorKind::IntegrityFailure,
        ErrorKind::Internal,
    ];
}

/// One command the user can copy and run verbatim, plus a line saying what it
/// does. The specification allows exactly one preferred fix per failure: offering a menu
/// of options is what makes error output unusable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Remedy {
    description: String,
    command: String,
}

impl Remedy {
    pub fn new(description: impl Into<String>, command: impl Into<String>) -> Self {
        Self {
            description: description.into(),
            command: command.into(),
        }
    }

    #[must_use]
    pub fn description(&self) -> &str {
        &self.description
    }

    #[must_use]
    pub fn command(&self) -> &str {
        &self.command
    }
}

/// The error type returned by every fallible Clift operation.
#[derive(Debug)]
pub struct CliftError {
    stage: Stage,
    kind: ErrorKind,
    message: String,
    remedy: Option<Remedy>,
    source: Option<Box<dyn Error + Send + Sync + 'static>>,
}

impl CliftError {
    pub fn new(stage: Stage, kind: ErrorKind, message: impl Into<String>) -> Self {
        Self {
            stage,
            kind,
            message: message.into(),
            remedy: None,
            source: None,
        }
    }

    /// Attaches the underlying cause. Adapters must call this: dropping the
    /// cause turns "SFTP subsystem missing" into "connection failed", which is
    /// exactly the loss of information the specification forbids.
    #[must_use]
    pub fn with_source(mut self, source: impl Error + Send + Sync + 'static) -> Self {
        self.source = Some(Box::new(source));
        self
    }

    #[must_use]
    pub fn with_remedy(mut self, remedy: Remedy) -> Self {
        self.remedy = Some(remedy);
        self
    }

    #[must_use]
    pub const fn stage(&self) -> Stage {
        self.stage
    }

    #[must_use]
    pub const fn kind(&self) -> ErrorKind {
        self.kind
    }

    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }

    #[must_use]
    pub fn remedy(&self) -> Option<&Remedy> {
        self.remedy.as_ref()
    }

    /// The full cause chain, outermost first. Rendered only under `--debug`;
    /// normal output shows the summary alone.
    #[must_use]
    pub fn cause_chain(&self) -> Vec<String> {
        let mut chain = vec![self.message.clone()];
        let mut current = self
            .source
            .as_ref()
            .map(|boxed| boxed.as_ref() as &dyn Error);
        while let Some(err) = current {
            chain.push(err.to_string());
            current = err.source();
        }
        chain
    }
}

impl fmt::Display for CliftError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl Error for CliftError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        self.source
            .as_ref()
            .map(|boxed| boxed.as_ref() as &(dyn Error + 'static))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug)]
    struct Inner;

    impl fmt::Display for Inner {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("inner cause")
        }
    }

    impl Error for Inner {}

    #[test]
    fn stage_names_are_distinct() {
        let mut seen: Vec<&str> = Stage::ALL.iter().map(|s| s.as_str()).collect();
        seen.sort_unstable();
        let count = seen.len();
        seen.dedup();
        assert_eq!(seen.len(), count, "two stages render to the same name");
    }

    #[test]
    fn error_without_source_has_no_chain_beyond_itself() {
        let err = CliftError::new(Stage::Config, ErrorKind::Config, "bad field");
        assert_eq!(err.cause_chain(), vec!["bad field".to_string()]);
        assert!(err.source().is_none());
        assert!(err.remedy().is_none());
    }

    #[test]
    fn source_is_preserved_in_the_chain() {
        let err = CliftError::new(Stage::Connect, ErrorKind::SshConnection, "cannot connect")
            .with_source(Inner);
        assert_eq!(
            err.cause_chain(),
            vec!["cannot connect".to_string(), "inner cause".to_string()]
        );
        assert!(err.source().is_some());
    }

    #[test]
    fn remedy_round_trips() {
        let err = CliftError::new(Stage::Connect, ErrorKind::SshConnection, "auth failed")
            .with_remedy(Remedy::new("Check the connection first:", "ssh core"));
        let remedy = err.remedy().unwrap();
        assert_eq!(remedy.description(), "Check the connection first:");
        assert_eq!(remedy.command(), "ssh core");
    }

    #[test]
    fn display_shows_the_summary_only() {
        let err = CliftError::new(Stage::Transfer, ErrorKind::Transfer, "size mismatch")
            .with_source(Inner);
        assert_eq!(err.to_string(), "size mismatch");
    }
}
