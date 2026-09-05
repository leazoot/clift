//! Exit codes, and the only mapping from [`ErrorKind`] onto them.
//!
//! The terminal integration reads the exit code and nothing else: 10 means
//! "paste normally", anything non-zero means "type nothing". A second
//! copy of this mapping anywhere in the workspace would eventually disagree
//! with this one and silently write Clift's error text into an agent's prompt,
//! so `scripts/check-architecture.sh` asserts the match arms exist only here.

use crate::error::ErrorKind;

/// The exit codes defined by the specification. No other code may be returned.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ExitCode {
    Success,
    /// Nothing to send; the terminal adapter performs a native paste.
    NoAttachment,
    Config,
    AmbiguousTarget,
    SshConnection,
    Transfer,
    ClipboardRead,
    RemoteDirectory,
    LimitExceeded,
    /// Universal Mode: the token cannot be redeemed.
    TokenUnusable,
    /// Universal Mode: the relay could not be reached.
    RelayUnavailable,
    /// Universal Mode: the bytes that arrived are not what they claim to be.
    IntegrityFailure,
    Internal,
}

impl ExitCode {
    pub const ALL: [ExitCode; 13] = [
        ExitCode::Success,
        ExitCode::NoAttachment,
        ExitCode::Config,
        ExitCode::AmbiguousTarget,
        ExitCode::SshConnection,
        ExitCode::Transfer,
        ExitCode::ClipboardRead,
        ExitCode::RemoteDirectory,
        ExitCode::LimitExceeded,
        ExitCode::TokenUnusable,
        ExitCode::RelayUnavailable,
        ExitCode::IntegrityFailure,
        ExitCode::Internal,
    ];

    /// The numeric value handed to the operating system.
    ///
    /// Converting this into the standard library's process exit type is left
    /// to `clift-cli`: `clift-core` must stay free of platform and process
    /// APIs, so it deals only in the numeric value.
    #[must_use]
    pub const fn as_u8(self) -> u8 {
        match self {
            ExitCode::Success => 0,
            ExitCode::NoAttachment => 10,
            ExitCode::Config => 20,
            ExitCode::AmbiguousTarget => 21,
            ExitCode::SshConnection => 22,
            ExitCode::Transfer => 23,
            ExitCode::ClipboardRead => 24,
            ExitCode::RemoteDirectory => 25,
            ExitCode::LimitExceeded => 26,
            ExitCode::TokenUnusable => 27,
            ExitCode::RelayUnavailable => 28,
            ExitCode::IntegrityFailure => 29,
            ExitCode::Internal => 30,
        }
    }
}

impl ErrorKind {
    /// The single source of truth for error-to-exit-code translation.
    #[must_use]
    pub const fn exit_code(self) -> ExitCode {
        match self {
            ErrorKind::NoAttachment => ExitCode::NoAttachment,
            ErrorKind::Config => ExitCode::Config,
            ErrorKind::AmbiguousTarget => ExitCode::AmbiguousTarget,
            ErrorKind::SshConnection => ExitCode::SshConnection,
            ErrorKind::Transfer => ExitCode::Transfer,
            ErrorKind::ClipboardRead => ExitCode::ClipboardRead,
            ErrorKind::RemoteDirectory => ExitCode::RemoteDirectory,
            ErrorKind::LimitExceeded => ExitCode::LimitExceeded,
            ErrorKind::TokenUnusable => ExitCode::TokenUnusable,
            ErrorKind::RelayUnavailable => ExitCode::RelayUnavailable,
            ErrorKind::IntegrityFailure => ExitCode::IntegrityFailure,
            ErrorKind::Internal => ExitCode::Internal,
        }
    }
}

impl crate::error::CliftError {
    #[must_use]
    pub const fn exit_code(&self) -> ExitCode {
        self.kind().exit_code()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::{CliftError, Stage};

    /// The specification lists these thirteen codes and no others. The three added in
    /// v2.0 sit at 27, 28 and 29: appended, never renumbered, so a script
    /// written against v1 keeps meaning what it meant.
    const FR_063: [u8; 13] = [0, 10, 20, 21, 22, 23, 24, 25, 26, 27, 28, 29, 30];

    #[test]
    fn exit_codes_match_fr_063_exactly() {
        let mut actual: Vec<u8> = ExitCode::ALL.iter().map(|code| code.as_u8()).collect();
        actual.sort_unstable();
        let mut expected = FR_063;
        expected.sort_unstable();
        assert_eq!(actual, expected.to_vec());
    }

    #[test]
    fn exit_codes_are_unique() {
        let mut seen: Vec<u8> = ExitCode::ALL.iter().map(|code| code.as_u8()).collect();
        seen.sort_unstable();
        let count = seen.len();
        seen.dedup();
        assert_eq!(seen.len(), count, "two variants share a numeric exit code");
    }

    #[test]
    fn every_error_kind_maps_to_its_documented_code() {
        let expected = [
            (ErrorKind::NoAttachment, 10),
            (ErrorKind::Config, 20),
            (ErrorKind::AmbiguousTarget, 21),
            (ErrorKind::SshConnection, 22),
            (ErrorKind::Transfer, 23),
            (ErrorKind::ClipboardRead, 24),
            (ErrorKind::RemoteDirectory, 25),
            (ErrorKind::LimitExceeded, 26),
            (ErrorKind::TokenUnusable, 27),
            (ErrorKind::RelayUnavailable, 28),
            (ErrorKind::IntegrityFailure, 29),
            (ErrorKind::Internal, 30),
        ];
        assert_eq!(
            expected.len(),
            ErrorKind::ALL.len(),
            "a new ErrorKind was added without an expected exit code"
        );
        for (kind, code) in expected {
            assert_eq!(kind.exit_code().as_u8(), code, "wrong code for {kind:?}");
        }
    }

    #[test]
    fn no_error_kind_maps_to_success() {
        for kind in ErrorKind::ALL {
            assert_ne!(
                kind.exit_code(),
                ExitCode::Success,
                "{kind:?} would report failure as success"
            );
        }
    }

    #[test]
    fn error_kinds_map_onto_distinct_codes() {
        let mut seen: Vec<u8> = ErrorKind::ALL
            .iter()
            .map(|kind| kind.exit_code().as_u8())
            .collect();
        seen.sort_unstable();
        let count = seen.len();
        seen.dedup();
        assert_eq!(seen.len(), count, "two error kinds share an exit code");
    }

    /// The nine codes v1 published. Universal Mode may append, and this is
    /// what stops it from doing anything else: renumber one of these and a
    /// key binding installed last month starts pasting error text into
    /// somebody's prompt.
    #[test]
    fn the_v1_exit_codes_still_mean_what_they_meant() {
        let v1 = [
            (ErrorKind::NoAttachment, 10),
            (ErrorKind::Config, 20),
            (ErrorKind::AmbiguousTarget, 21),
            (ErrorKind::SshConnection, 22),
            (ErrorKind::Transfer, 23),
            (ErrorKind::ClipboardRead, 24),
            (ErrorKind::RemoteDirectory, 25),
            (ErrorKind::LimitExceeded, 26),
            (ErrorKind::Internal, 30),
        ];
        for (kind, code) in v1 {
            assert_eq!(kind.exit_code().as_u8(), code, "v1 code for {kind:?} moved");
        }
        assert_eq!(ExitCode::Success.as_u8(), 0);
    }

    #[test]
    fn error_exposes_the_code_of_its_kind() {
        let err = CliftError::new(Stage::Staging, ErrorKind::LimitExceeded, "too many files");
        assert_eq!(err.exit_code().as_u8(), 26);
    }
}
