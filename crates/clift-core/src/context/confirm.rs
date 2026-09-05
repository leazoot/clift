//! Whether Clift may ask before doing something.
//!
//! A non-interactive run must never wait on a prompt nobody can see.
//! A cron job or an editor plugin that hits a hidden question does not fail, it
//! hangs, which is worse than failing.

use crate::error::{CliftError, ErrorKind, Remedy, Stage};

/// How a confirmation is to be obtained.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Confirmation {
    /// Ask the user and wait for an answer.
    Ask,
    /// The user said yes in advance, on the command line.
    AlreadyGiven,
}

/// Decides how an action that needs confirmation should proceed.
///
/// # Errors
/// Fails immediately when there is nobody to ask and no `--yes`, naming the
/// flag that would have made the command work.
pub fn confirmation_for(
    action: &str,
    interactive: bool,
    assume_yes: bool,
) -> Result<Confirmation, CliftError> {
    if assume_yes {
        return Ok(Confirmation::AlreadyGiven);
    }
    if interactive {
        return Ok(Confirmation::Ask);
    }
    Err(CliftError::new(
        Stage::Config,
        ErrorKind::Config,
        format!("{action} needs confirmation, and this is not an interactive terminal"),
    )
    .with_remedy(Remedy::new(
        "Confirm on the command line instead:",
        "add --yes to the command",
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_interactive_run_asks() {
        assert_eq!(
            confirmation_for("setting up core", true, false).unwrap(),
            Confirmation::Ask
        );
    }

    #[test]
    fn yes_on_the_command_line_answers_in_advance() {
        assert_eq!(
            confirmation_for("setting up core", true, true).unwrap(),
            Confirmation::AlreadyGiven
        );
        assert_eq!(
            confirmation_for("setting up core", false, true).unwrap(),
            Confirmation::AlreadyGiven,
            "--yes is exactly what makes a non-interactive run possible"
        );
    }

    /// The whole point: fail, do not wait.
    #[test]
    fn a_non_interactive_run_without_yes_fails_immediately() {
        let error = confirmation_for("setting up core", false, false)
            .expect_err("there is nobody to answer");
        assert_eq!(error.exit_code().as_u8(), 20);
        assert!(error.to_string().contains("setting up core"), "{error}");
        assert!(
            error
                .remedy()
                .is_some_and(|remedy| remedy.command().contains("--yes")),
            "the message must name the flag that would have worked"
        );
    }
}
