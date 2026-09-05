//! `clift hotkey`: the helper the specification asks for.
//!
//! One combination, registered with the operating system, that runs the same
//! paste `clift paste` runs. It exists because the alternative was asking every
//! user to install a different third-party tool on each platform, which is not
//! what "works in any terminal" can be allowed to mean.
//!
//! It is the one long-running thing Clift has, and the cost is admitted rather
//! than hidden: a process that lives between key presses is a process, and the
//! "no resident process" line in the performance targets does not survive it.
//! What keeps that honest is that it does nothing until the user asked for it,
//! by starting it, and stops the moment they stop it.
//!
//! Two things it deliberately does not do. It does not watch the clipboard --
//! nothing is read until the key is pressed. And it does not report a paste it
//! did not perform: when the machine will not let Clift send a keystroke, the
//! token goes on the clipboard instead and the user is told so, every time,
//! rather than being left waiting for something that already failed.
//!
//! One key, two directions. What a press does is decided by what is on the
//! clipboard when it happens: an image goes out, a token that came back off a
//! server's terminal comes in, and ordinary text is left alone. The rule itself
//! is in `clift_core::hotkey::token_to_redeem`, next to the reason it is
//! written the way it is.

use crate::cmd::{fetch, paste};
use crate::output::Reporter;
use clift_core::config;
use clift_core::error::{CliftError, ErrorKind, Remedy, Stage};
use clift_core::hotkey::{self, Hotkey};
use clift_core::ports::{ClipboardSnapshot, ClipboardSource};
use clift_inject::{Availability, autostart};

/// # Errors
/// Fails when the combination is unreadable, when the operating system will not
/// give it to Clift, and when a login registration cannot be written.
pub fn run(
    key: Option<&str>,
    install: bool,
    uninstall: bool,
    reporter: &Reporter,
) -> Result<(), CliftError> {
    if uninstall {
        return remove_login_item(reporter);
    }

    let combination = combination(key, reporter)?;

    if install {
        if key.is_some() {
            // Refused rather than honoured, because honouring it would put the
            // combination in two places: baked into the launch agent and
            // written in the configuration file. The next person to change one
            // of them would be surprised by the other.
            return Err(CliftError::new(
                Stage::Config,
                ErrorKind::Config,
                "--key configures one run; a login item reads the configuration file",
            )
            .with_remedy(Remedy::new(
                "Save the combination first, then install:",
                "clift config set hotkey.combination <combination>",
            )));
        }
        return add_login_item(&combination, reporter);
    }

    listen(&combination, reporter)
}

/// Which combination this run uses: the flag, then the file, then the default.
pub(crate) fn combination(key: Option<&str>, reporter: &Reporter) -> Result<Hotkey, CliftError> {
    if let Some(spec) = key {
        return Hotkey::parse(spec).map_err(|error| {
            CliftError::new(Stage::Config, ErrorKind::Config, error.to_string()).with_remedy(
                Remedy::new(
                    "Combinations look like this:",
                    "clift hotkey --key cmd+shift+v",
                ),
            )
        });
    }

    let path = config::io::default_config_path()?;
    let loaded = config::io::load(&path)?;
    for warning in &loaded.warnings {
        reporter.warn(warning);
    }
    Ok(loaded
        .config
        .hotkey()
        .copied()
        .unwrap_or_else(hotkey::default_combination))
}

/// Registers the combination and pastes on every press, until interrupted.
fn listen(combination: &Hotkey, reporter: &Reporter) -> Result<(), CliftError> {
    reporter.success(&format!(
        "Listening for {}. Press it in any window.",
        combination.render()
    ));
    if reporter.interactive() {
        // Only where it is true. Under launchd this line is the first thing in
        // the log file, and there is no terminal to press Ctrl+C in; how to
        // stop that one was printed by `--install`.
        reporter.verbose("Ctrl+C to stop");
    }
    if let Some(warning) = combination.warning() {
        reporter.warn(&warning);
    }
    // Asked before the first press rather than after it: where the platform
    // has a dialog for this, the user sees it now, while the helper is fresh
    // in their mind, and not as a mystery when a paste they expected does not
    // arrive. Where it has none, this is the plain check.
    announce(&clift_inject::request_permission(), reporter);

    let mut previous = clift_inject::availability();
    clift_inject::hotkey::listen(combination, &mut || {
        // Re-checked every press, so granting the permission takes effect
        // without restarting the helper -- and so does revoking it.
        let now = clift_inject::availability();
        if now != previous {
            announce(&now, reporter);
            previous = now.clone();
        }
        press(now.is_ready(), reporter);
    })
}

/// One press, in whichever direction the clipboard calls for.
///
/// The clipboard is read exactly once, here, and the snapshot is handed on to
/// whichever half runs. That is not tidiness: reading it twice would write the
/// image to a second temporary file on every outward press, and the second read
/// could disagree with the first if the user copied something in between --
/// which would mean deciding on one clipboard and acting on another.
fn press(can_inject: bool, reporter: &Reporter) {
    // Held for the whole function: the adapter owns the temporary file behind
    // any image in the snapshot, and dropping it early would delete the file
    // the paste is about to read.
    let clipboard = clift_clipboard::SystemClipboard::new();
    let snapshot = match clipboard.read_snapshot() {
        Ok(snapshot) => snapshot,
        Err(error) => {
            reporter.error(&error);
            return;
        }
    };

    if let Some(token) = hotkey::token_to_redeem(&snapshot) {
        // The return trip. No `--inject` here and nothing typed: the result is
        // a picture, and a picture goes on the clipboard or nowhere.
        report(fetch::redeem(&token, false, true, reporter), reporter);
        return;
    }

    if can_inject && clift_inject::modifiers_are_held() {
        // Ordinary: the key that got us here is still down. Worth a line under
        // `--verbose` because when the paste does go wrong, the first question
        // is whether the keystroke was mangled on the way out.
        reporter.verbose("waiting for the hotkey to be released before pasting");
    }
    let outcome = paste::run(
        None,
        None,
        !can_inject,
        can_inject,
        &AlreadyRead(snapshot),
        reporter,
    );
    report(outcome, reporter);
}

/// The clipboard this press already looked at.
///
/// A source that reads nothing, so that the decision above and the work below
/// are made about the same clipboard.
struct AlreadyRead(ClipboardSnapshot);

impl ClipboardSource for AlreadyRead {
    fn read_snapshot(&self) -> Result<ClipboardSnapshot, CliftError> {
        Ok(self.0.clone())
    }
}

/// What the helper does with the outcome of a press, in either direction.
fn report(outcome: Result<(), CliftError>, reporter: &Reporter) {
    match outcome {
        Ok(()) => {}
        // Not a failure. The user pressed the key with text on the clipboard,
        // or with nothing on it at all, and the helper has nothing to do.
        Err(error) if error.kind() == ErrorKind::NoAttachment => {
            reporter.warn("nothing on the clipboard to send; the key did nothing");
        }
        // Reported and swallowed: one failed paste must not take the helper
        // down, or a single unreachable relay would leave the user pressing a
        // key that has silently stopped existing.
        Err(error) => reporter.error(&error),
    }
}

/// Says whether the key will paste for the user or only load their clipboard.
///
/// The wording here is not `--inject`'s, and the difference is not cosmetic.
/// macOS grants Accessibility to whichever program is *responsible* for the
/// process, and for this helper that is two different programs depending on how
/// it was started: the terminal, when the user runs it themselves, and the
/// clift binary itself, when launchd starts it at login. Telling a user with a
/// login item to authorise their terminal would send them to add the wrong
/// entry and leave the key still not working -- which was the first thing this
/// helper actually did when it was tried.
fn announce(availability: &Availability, reporter: &Reporter) {
    if availability.is_ready() {
        reporter.verbose("each press will type the instruction into the focused window");
        return;
    }

    let mut message = String::from(
        "macOS has not allowed Clift to send keystrokes, so each press will put the text on \
         your clipboard for you to paste yourself.",
    );
    if let Availability::Unsupported(text) = availability {
        // Not a permission that can be granted: a platform with no
        // implementation. Say that instead of sending the user to a settings
        // pane that will not help.
        message = format!(
            "{text}, so each press will put the text on your clipboard for you to paste yourself."
        );
    } else if let Ok(program) = std::env::current_exe() {
        message.push_str(&format!(
            "\nTo change that, add the program that runs it under System Settings > Privacy & \
             Security > Accessibility: {} if it starts at login, or your terminal if you run it \
             yourself.",
            program.display()
        ));
    }
    reporter.warn(&message);
}

fn add_login_item(combination: &Hotkey, reporter: &Reporter) -> Result<(), CliftError> {
    // Whatever binary is running now, so a helper installed from a build in
    // `target/release` starts that same build rather than one on PATH that may
    // be older -- a trap this project has already fallen into once.
    let program = std::env::current_exe().map_err(|error| {
        CliftError::new(
            Stage::Injection,
            ErrorKind::Config,
            "cannot determine which clift binary is running",
        )
        .with_source(error)
    })?;

    let installed = autostart::install(&program, &["hotkey".to_string()])?;
    reporter.success(&format!(
        "{} will start at login and is running now.",
        combination.render()
    ));
    // The program comes first, and it is here because leaving it out cost real
    // time: a user with an older `clift` earlier on their PATH installed that
    // one, went to authorise the one they thought they had installed, and got a
    // helper that quietly could not paste. It is also the exact path macOS wants
    // in its Accessibility list.
    reporter.success(&format!("  program:    {}", installed.program.display()));
    reporter.success(&format!("  definition: {}", installed.definition.display()));
    reporter.success(&format!("  log:        {}", installed.log.display()));
    reporter.success("  remove it with: clift hotkey --uninstall");
    Ok(())
}

fn remove_login_item(reporter: &Reporter) -> Result<(), CliftError> {
    match autostart::uninstall()? {
        Some(path) => {
            reporter.success(&format!("Removed {}.", path.display()));
        }
        None => reporter.success("Nothing was installed; nothing to remove."),
    }
    Ok(())
}
