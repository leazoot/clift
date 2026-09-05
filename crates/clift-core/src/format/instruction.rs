//! The `instruction` profile.
//!
//! One file:
//!
//! ```text
//! Please inspect this file: '/home/dev/.cache/clift/inbox/2026-08-30/ab.../shot.png'
//! ```
//!
//! Several:
//!
//! ```text
//! Please inspect these files:
//! - '/home/dev/.cache/clift/inbox/2026-08-30/ab.../design.png'
//! - '/home/dev/.cache/clift/inbox/2026-08-30/ab.../requirements.pdf'
//! ```
//!
//! Four constraints shape it, and each of them is a thing that would go wrong
//! otherwise:
//!
//! - **no local path**: the agent is on the far side and cannot open one;
//! - **no agent's name**: text that says "Claude" is wrong in front of any
//!   other agent, and Clift does not know which one is listening;
//! - **no trailing newline**: a newline is the submit key in most prompts, and
//!   sending on the user's behalf takes the decision away from them;
//! - **quoted paths**: a path with a space in it must not come apart if the
//!   agent hands the text to a shell.

use crate::domain::RemotePath;

/// Renders the text a user pastes.
///
/// The empty case cannot arise from a successful send -- a batch with no
/// attachments never gets this far -- but it is rendered as an empty string
/// rather than as an instruction to inspect nothing.
#[must_use]
pub fn render(paths: &[RemotePath]) -> String {
    match paths {
        [] => String::new(),
        [only] => format!("Please inspect this file: {}", quote(only.as_str())),
        many => {
            let mut text = String::from("Please inspect these files:");
            for path in many {
                text.push_str("\n- ");
                text.push_str(&quote(path.as_str()));
            }
            text
        }
    }
}

/// Quotes a path so that a shell on the far side reads it as one word.
///
/// Single quotes, with the POSIX escape for a single quote inside them:
/// `it's.png` becomes `'it'\''s.png'`. This is ugly and it is correct; the
/// alternatives -- double quotes, or backslash escaping -- both leave
/// characters the shell still expands.
pub(crate) fn quote(path: &str) -> String {
    let mut quoted = String::with_capacity(path.len() + 2);
    quoted.push('\'');
    for character in path.chars() {
        if character == '\'' {
            // Close the quoted run, emit an escaped quote, open a new run.
            quoted.push_str("'\\''");
        } else {
            quoted.push(character);
        }
    }
    quoted.push('\'');
    quoted
}

#[cfg(test)]
mod tests {
    use super::*;

    fn path(value: &str) -> RemotePath {
        RemotePath::new(value).unwrap_or_else(|error| panic!("bad test path: {error}"))
    }

    #[test]
    fn one_file_matches_the_example_in_the_requirements() {
        let text = render(&[path("/home/user/.cache/clift/inbox/2026-08-30/ab/shot.png")]);
        assert_eq!(
            text,
            "Please inspect this file: '/home/user/.cache/clift/inbox/2026-08-30/ab/shot.png'"
        );
    }

    #[test]
    fn several_files_match_the_example_in_the_requirements() {
        let text = render(&[
            path("/home/user/.cache/clift/inbox/2026-08-30/ab/design.png"),
            path("/home/user/.cache/clift/inbox/2026-08-30/ab/requirements.pdf"),
        ]);
        assert_eq!(
            text,
            concat!(
                "Please inspect these files:\n",
                "- '/home/user/.cache/clift/inbox/2026-08-30/ab/design.png'\n",
                "- '/home/user/.cache/clift/inbox/2026-08-30/ab/requirements.pdf'"
            )
        );
    }

    /// A newline at the end is the submit key in most agent prompts. Adding one
    /// would send the message for the user, who may have wanted to say more.
    #[test]
    fn the_text_never_ends_in_a_newline() {
        for paths in [
            vec![path("/home/dev/inbox/a.png")],
            vec![path("/home/dev/inbox/a.png"), path("/home/dev/inbox/b.png")],
        ] {
            let text = render(&paths);
            assert!(!text.ends_with('\n'), "{text:?}");
            assert!(!text.ends_with('\r'), "{text:?}");
        }
    }

    /// A path with a space, a quote or a dollar sign must survive being handed
    /// to a shell.
    #[test]
    fn awkward_paths_come_out_as_one_shell_word() {
        let cases = [
            (
                "/home/dev/inbox/my screenshot.png",
                "'/home/dev/inbox/my screenshot.png'",
            ),
            ("/home/dev/inbox/名字.png", "'/home/dev/inbox/名字.png'"),
            ("/home/dev/inbox/$HOME.png", "'/home/dev/inbox/$HOME.png'"),
            (
                "/home/dev/inbox/back\\slash.png",
                "'/home/dev/inbox/back\\slash.png'",
            ),
            (
                "/home/dev/inbox/it's here.png",
                "'/home/dev/inbox/it'\\''s here.png'",
            ),
        ];
        for (input, expected) in cases {
            assert_eq!(quote(input), expected, "{input}");
        }
    }

    /// Nothing local, and nothing that names an agent.
    #[test]
    fn the_text_names_no_local_path_and_no_agent() {
        let text = render(&[path("/home/dev/.cache/clift/inbox/2026-08-30/ab/shot.png")]);
        for forbidden in [
            "/Users/", "/tmp/", "C:\\", "Claude", "GPT", "Codex", "Gemini",
        ] {
            assert!(!text.contains(forbidden), "{text}");
        }
    }

    #[test]
    fn no_paths_renders_nothing_rather_than_an_empty_instruction() {
        assert_eq!(render(&[]), "");
    }
}
