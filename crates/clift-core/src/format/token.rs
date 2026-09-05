//! The line Universal Mode puts in front of the agent.
//!
//! Fast Mode pastes a path, because the file is already there. Universal Mode
//! pastes an instruction, because it is not: the agent has to run one command
//! before there is anything to look at.
//!
//! ```text
//! Attachment: clift fetch 'clift://v1/...#...'
//! ```
//!
//! It used to be a sentence ("Fetch and inspect this attachment by running:
//! ... --print-path"). The sentence was there for an agent with no standing
//! instructions, and it doubled the length of a line whose other half is a
//! token that cannot be shortened. Every server set up the documented way has
//! the instructions; for the rest, a command is still a command, and `clift
//! fetch` now prints the path without being asked.
//!
//! The constraints are the same four that shape the `instruction` profile, plus
//! one that is new:
//!
//! - **no agent's name** -- "Claude" is wrong in front of Codex, and Clift does
//!   not know which one is listening;
//! - **no trailing newline** -- a newline submits, and that is the user's key
//!   to press;
//! - **the token is quoted** -- and here it is not a nicety. A token contains
//!   `#`, which starts a comment in every POSIX shell: unquoted, the agent
//!   would run `clift fetch clift://v1/<id>` with the key silently discarded,
//!   and get "the token has no key material" for its trouble;
//! - **one command, not a description of one** -- an agent that has to work out
//!   what to run will sometimes work out something else.

use crate::format::instruction::quote;
use crate::universal::Token;

/// Renders the instruction for one published object.
///
/// `count` only changes the wording; the command is the same either way,
/// because one token carries the whole batch.
#[must_use]
pub fn render(token: &Token, count: usize) -> String {
    let noun = if count == 1 {
        "Attachment"
    } else {
        "Attachments"
    };
    format!("{noun}: clift fetch {}", quote(&token.expose()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::universal::token::{OBJECT_ID_BYTES, ObjectId, SEAL_KEY_BYTES, SealKey};

    fn token() -> Token {
        Token::new(
            ObjectId::from_bytes([1; OBJECT_ID_BYTES]),
            SealKey::from_bytes([2; SEAL_KEY_BYTES]),
        )
    }

    #[test]
    fn the_command_is_runnable_and_the_token_survives_a_shell() {
        let text = render(&token(), 1);
        assert!(text.contains("clift fetch '"), "{text}");
        assert!(text.ends_with('\''), "nothing after the token: {text}");
        assert!(!text.contains("--print-path"), "{text}");
        // The token is inside single quotes, so the '#' cannot start a comment.
        let quoted = format!("'{}'", token().expose());
        assert!(text.contains(&quoted), "{text}");
    }

    #[test]
    fn nothing_is_submitted_on_the_users_behalf() {
        assert!(!render(&token(), 1).ends_with('\n'));
        assert!(!render(&token(), 3).contains('\n'));
    }

    #[test]
    fn no_agent_is_named() {
        let text = render(&token(), 1).to_ascii_lowercase();
        for name in [
            "claude", "codex", "gemini", "gpt", "copilot", "aider", "opencode",
        ] {
            assert!(!text.contains(name), "the text names {name}");
        }
    }

    #[test]
    fn several_attachments_read_as_several() {
        assert!(render(&token(), 1).starts_with("Attachment: "));
        assert!(render(&token(), 4).starts_with("Attachments: "));
    }
}
