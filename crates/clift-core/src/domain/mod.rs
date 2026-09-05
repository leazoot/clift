//! Domain newtypes whose invariants make illegal states unrepresentable.
//!
//! Every type here has private fields and a validating constructor. There is
//! deliberately no `from_unchecked`, no public field and no `From<String>`:
//! a bypass would move the invariant back into "every call site remembers to
//! check", which is what these types exist to avoid.

mod attachment;
mod batch_id;
mod limits;
pub mod local_path;
mod remote_path;
mod safe_name;
mod target_name;

pub use attachment::{FileKind, LocalAttachment};
pub use batch_id::{BYTES as BATCH_ID_BYTES, BatchId};
pub use limits::Limits;
pub use local_path::LocalPath;
pub use remote_path::RemotePath;
pub use safe_name::{BatchNames, MAX_LEN as MAX_FILE_NAME_LEN, SafeFileName};
pub use target_name::TargetName;

use crate::error::{CliftError, ErrorKind, Stage};
use std::error::Error;
use std::fmt;

/// Why a domain value was rejected.
///
/// Domain types do not know which pipeline stage asked for them: the same
/// rejected file name means "clipboard read failed" in one flow and "staging
/// failed" in another. Attribution is therefore left to the caller through
/// [`DomainError::into_clift`], which also keeps this error in the cause chain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DomainError {
    subject: &'static str,
    reason: String,
}

impl DomainError {
    pub(crate) fn new(subject: &'static str, reason: impl Into<String>) -> Self {
        Self {
            subject,
            reason: reason.into(),
        }
    }

    #[must_use]
    pub fn subject(&self) -> &'static str {
        self.subject
    }

    #[must_use]
    pub fn reason(&self) -> &str {
        &self.reason
    }

    /// Wraps this rejection into a [`CliftError`] attributed to the stage and
    /// kind the caller knows it belongs to.
    #[must_use]
    pub fn into_clift(self, stage: Stage, kind: ErrorKind) -> CliftError {
        let message = self.to_string();
        CliftError::new(stage, kind, message).with_source(self)
    }
}

impl fmt::Display for DomainError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "invalid {}: {}", self.subject, self.reason)
    }
}

impl Error for DomainError {}

/// Shared rejection rule: control characters break terminal output, remote
/// path handling and configuration files alike, so no domain value accepts them.
pub(crate) fn reject_control_characters(
    subject: &'static str,
    value: &str,
) -> Result<(), DomainError> {
    if value.chars().any(char::is_control) {
        return Err(DomainError::new(subject, "contains a control character"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn domain_error_attribution_is_chosen_by_the_caller() {
        let err = DomainError::new("target name", "must not be empty")
            .into_clift(Stage::Config, ErrorKind::Config);
        assert_eq!(err.stage(), Stage::Config);
        assert_eq!(err.exit_code().as_u8(), 20);
        assert_eq!(
            err.cause_chain(),
            vec![
                "invalid target name: must not be empty".to_string(),
                "invalid target name: must not be empty".to_string(),
            ],
            "the domain error must stay in the cause chain"
        );
    }

    #[test]
    fn control_characters_are_rejected() {
        assert!(reject_control_characters("thing", "ok").is_ok());
        assert!(reject_control_characters("thing", "bad\u{7}").is_err());
        assert!(reject_control_characters("thing", "line\nbreak").is_err());
    }
}
