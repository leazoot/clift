//! Output channel discipline.
//!
//! This is the single most consequential rule in the CLI. A caller
//! reads stdout and types it into the agent's prompt, so anything that is not a
//! machine result or insertion text must go to stderr. A stray progress line on
//! stdout does not look like a bug: it looks like Clift typing noise into
//! someone's conversation with their agent.

use clift_core::error::CliftError;
use std::io::{IsTerminal, Write};

/// Decides where each kind of message goes and whether it is colored.
pub struct Reporter {
    json: bool,
    verbose: bool,
    debug: bool,
    color: bool,
    interactive: bool,
}

impl Reporter {
    #[must_use]
    pub fn new(json: bool, verbose: bool, debug: bool) -> Self {
        Self {
            json,
            verbose,
            debug,
            color: color_enabled(),
            interactive: std::io::stderr().is_terminal(),
        }
    }

    #[must_use]
    pub const fn json(&self) -> bool {
        self.json
    }

    /// Whether a person is watching stderr right now.
    ///
    /// Only then is an animation worth drawing: redirected, it would fill a log
    /// file with escape sequences and defeat comparing one run against another.
    /// `--json` does not turn it off, because the document goes to stdout and
    /// the spinner erases itself from stderr.
    #[must_use]
    pub const fn interactive(&self) -> bool {
        self.interactive
    }

    /// Something the user should know but which does not stop the operation.
    pub fn warn(&self, message: &str) {
        let label = self.paint("warning", Tone::Warn);
        self.to_stderr(&format!("{label}: {message}\n"));
    }

    /// The result line of a successful operation. Human readable, so stderr:
    /// only machine results and insertion text may reach stdout.
    pub fn success(&self, message: &str) {
        self.to_stderr(&format!("{message}\n"));
    }

    /// A stage or timing note, shown only under `--verbose` or `--debug`.
    pub fn verbose(&self, message: &str) {
        if self.verbose || self.debug {
            self.to_stderr(&format!("{message}\n"));
        }
    }

    /// Renders a failure in the three-part shape of the specification: what failed, how
    /// to check it, how to retry.
    pub fn error(&self, error: &CliftError) {
        let label = self.paint("error", Tone::Fail);
        let mut text = format!("{label}: {} failed: {}\n", error.stage(), error.message());

        if let Some(remedy) = error.remedy() {
            text.push('\n');
            text.push_str(remedy.description());
            text.push('\n');
            text.push_str("  ");
            text.push_str(&self.paint(remedy.command(), Tone::Command));
            text.push('\n');
        }

        if self.debug {
            text.push_str("\ncaused by:\n");
            for (depth, cause) in error.cause_chain().iter().enumerate().skip(1) {
                text.push_str(&format!("{:indent$}{cause}\n", "", indent = depth * 2));
            }
        }

        self.to_stderr(&text);
    }

    /// The one and only thing allowed on stdout in `--json` mode.
    ///
    /// Written without a trailing newline: the contract is that stdout is
    /// exactly one JSON document and nothing else.
    ///
    /// # Errors
    /// Fails if the value cannot be serialized or stdout cannot be written.
    pub fn machine(&self, value: &serde_json::Value) -> std::io::Result<()> {
        let rendered = serde_json::to_string(value)?;
        let mut stdout = std::io::stdout().lock();
        stdout.write_all(rendered.as_bytes())?;
        stdout.flush()
    }

    /// Text destined for the agent's prompt. The only other legitimate use of
    /// stdout.
    ///
    /// # Errors
    /// Fails if stdout cannot be written.
    pub fn insertion_text(&self, text: &str) -> std::io::Result<()> {
        let mut stdout = std::io::stdout().lock();
        stdout.write_all(text.as_bytes())?;
        stdout.flush()
    }

    fn to_stderr(&self, text: &str) {
        // A closed stderr must not turn a successful send into a failure.
        let _ = std::io::stderr().lock().write_all(text.as_bytes());
    }

    /// `text` in a colour, or `text` unchanged when colour is off. Colour is
    /// for telling pass from warn from fail and for picking out a command to
    /// copy; it is never decoration (the output rules).
    #[must_use]
    pub fn paint(&self, text: &str, tone: Tone) -> String {
        if !self.color {
            return text.to_string();
        }
        format!("{}{text}{}", tone.prefix(), Tone::RESET)
    }
}

/// The few things colour is allowed to mean.
#[derive(Debug, Clone, Copy)]
pub enum Tone {
    Heading,
    Ok,
    Warn,
    Fail,
    Command,
}

impl Tone {
    const RESET: &'static str = "\u{1b}[0m";

    const fn prefix(self) -> &'static str {
        match self {
            Tone::Heading => "\u{1b}[1m",
            Tone::Ok => "\u{1b}[32m",
            Tone::Warn => "\u{1b}[33m",
            Tone::Fail => "\u{1b}[31m",
            Tone::Command => "\u{1b}[1m",
        }
    }
}

/// Color is off unless stderr is a terminal, and off regardless when `NO_COLOR`
/// is set to anything at all, per the NO_COLOR convention.
fn color_enabled() -> bool {
    if std::env::var_os("NO_COLOR").is_some() {
        return false;
    }
    std::io::stderr().is_terminal()
}
