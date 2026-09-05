//! Questions, asked on stderr and answered on stdin.
//!
//! The channel rule holds here as everywhere else: a prompt is human-readable
//! text, so it goes to stderr. A question printed to stdout would be typed
//! into an agent's prompt by whatever called Clift, which is worse than no
//! question at all.
//!
//! Whether to ask at all is decided by the caller, before anything is written:
//! `clift-core::context::confirmation_for` refuses on a non-interactive stdin
//! rather than waiting on a prompt nobody will see. This module
//! assumes that decision has been made and only does the asking, which is why
//! it takes the streams as parameters and can be exercised with a scripted
//! reader.

use clift_core::error::{CliftError, ErrorKind, Stage};
use std::io::{BufRead, Write};

/// One place to ask questions and print answers-in-progress.
pub struct Console<'a> {
    input: &'a mut dyn BufRead,
    output: &'a mut dyn Write,
}

impl<'a> Console<'a> {
    pub fn new(input: &'a mut dyn BufRead, output: &'a mut dyn Write) -> Self {
        Self { input, output }
    }

    /// One line of text, with its newline. Write failures are ignored: a
    /// closed stderr must not turn the answers already given into an error.
    pub fn say(&mut self, text: &str) {
        let _ = writeln!(self.output, "{text}");
        let _ = self.output.flush();
    }

    /// A yes/no question. Enter takes the default, which the prompt shows in
    /// capitals the way every Unix tool does.
    ///
    /// # Errors
    /// Fails when the input ends or cannot be read.
    pub fn confirm(&mut self, question: &str, default_yes: bool) -> Result<bool, CliftError> {
        let hint = if default_yes { "[Y/n]" } else { "[y/N]" };
        loop {
            let _ = write!(self.output, "{question} {hint} ");
            let _ = self.output.flush();
            let answer = self.read_answer()?.to_ascii_lowercase();
            match answer.as_str() {
                "" => return Ok(default_yes),
                "y" | "yes" => return Ok(true),
                "n" | "no" => return Ok(false),
                _ => self.say("Please answer y or n."),
            }
        }
    }

    /// A numbered choice. Returns the index into `options`; Enter takes
    /// `default`. An answer outside the list is refused and the question is
    /// asked again, so a typo cannot silently pick something.
    ///
    /// # Errors
    /// Fails when the input ends or cannot be read.
    pub fn choose(
        &mut self,
        question: &str,
        options: &[String],
        default: usize,
    ) -> Result<usize, CliftError> {
        self.say(question);
        for (index, option) in options.iter().enumerate() {
            self.say(&format!("  {}) {option}", index + 1));
        }
        loop {
            let _ = write!(self.output, "Choice [{}]: ", default + 1);
            let _ = self.output.flush();
            let answer = self.read_answer()?;
            if answer.is_empty() {
                return Ok(default);
            }
            match answer.parse::<usize>() {
                Ok(number) if (1..=options.len()).contains(&number) => return Ok(number - 1),
                _ => self.say(&format!("Please answer 1 to {}.", options.len())),
            }
        }
    }

    /// A free-text answer, trimmed. Empty is a valid answer; the caller
    /// decides what it means.
    ///
    /// # Errors
    /// Fails when the input ends or cannot be read.
    pub fn ask(&mut self, prompt: &str) -> Result<String, CliftError> {
        let _ = write!(self.output, "{prompt} ");
        let _ = self.output.flush();
        self.read_answer()
    }

    fn read_answer(&mut self) -> Result<String, CliftError> {
        let mut line = String::new();
        let read = self.input.read_line(&mut line).map_err(|error| {
            CliftError::new(
                Stage::Config,
                ErrorKind::Config,
                "could not read the answer from the terminal",
            )
            .with_source(error)
        })?;
        if read == 0 {
            return Err(CliftError::new(
                Stage::Config,
                ErrorKind::Config,
                "the input ended before the question was answered",
            ));
        }
        Ok(line.trim().to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn options() -> Vec<String> {
        vec!["first".to_string(), "second".to_string()]
    }

    #[test]
    fn enter_takes_the_default_and_the_prompt_shows_which_it_is() {
        let mut input = Cursor::new(b"\n\n".to_vec());
        let mut output = Vec::new();
        let mut console = Console::new(&mut input, &mut output);
        assert!(console.confirm("Go on?", true).unwrap());
        assert!(!console.confirm("Go on?", false).unwrap());
        let shown = String::from_utf8(output).unwrap();
        assert!(shown.contains("Go on? [Y/n] "), "{shown}");
        assert!(shown.contains("Go on? [y/N] "), "{shown}");
    }

    #[test]
    fn a_choice_outside_the_list_is_asked_again_not_guessed() {
        let mut input = Cursor::new(b"7\nzero\n2\n".to_vec());
        let mut output = Vec::new();
        let mut console = Console::new(&mut input, &mut output);
        assert_eq!(console.choose("Which?", &options(), 0).unwrap(), 1);
        let shown = String::from_utf8(output).unwrap();
        assert_eq!(shown.matches("Please answer 1 to 2.").count(), 2, "{shown}");
        assert!(shown.contains("  1) first"), "{shown}");
        assert!(shown.contains("Choice [1]: "), "{shown}");
    }

    #[test]
    fn an_empty_choice_takes_the_default() {
        let mut input = Cursor::new(b"\n".to_vec());
        let mut output = Vec::new();
        let mut console = Console::new(&mut input, &mut output);
        assert_eq!(console.choose("Which?", &options(), 1).unwrap(), 1);
    }

    #[test]
    fn input_that_ends_is_an_error_not_an_answer() {
        let mut input = Cursor::new(Vec::new());
        let mut output = Vec::new();
        let mut console = Console::new(&mut input, &mut output);
        let error = console.ask("Name:").unwrap_err();
        assert!(error.message().contains("ended"), "{error}");
        assert_eq!(error.stage(), Stage::Config);
    }

    #[test]
    fn free_text_is_trimmed_and_may_be_empty() {
        let mut input = Cursor::new(b"  https://relay.example  \n\n".to_vec());
        let mut output = Vec::new();
        let mut console = Console::new(&mut input, &mut output);
        assert_eq!(console.ask("Relay:").unwrap(), "https://relay.example");
        assert_eq!(console.ask("Relay:").unwrap(), "");
    }

    #[test]
    fn nothing_is_written_to_the_input_side() {
        // The prompt goes to the output stream only; the reader is never
        // written to, which is what keeps stdout clean when output is stderr.
        let mut input = Cursor::new(b"y\n".to_vec());
        let mut output = Vec::new();
        let mut console = Console::new(&mut input, &mut output);
        assert!(console.confirm("Ok?", false).unwrap());
        assert_eq!(input.get_ref(), b"y\n");
    }
}
