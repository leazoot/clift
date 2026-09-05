//! `clift setup` with no host: the first-time conversation.
//!
//! The installer ends by starting this, so a person who ran one line has a
//! working Clift a few questions later rather than a binary and a reading
//! assignment. Everything it does is something `clift config set`, `clift
//! setup <host>` and `clift hotkey --install` already do; it only asks in the
//! right order and checks the answers, so nothing here is a second way to
//! configure Clift.
//!
//! Two rules shape it:
//!
//! - **It refuses without a terminal.** A question nobody can answer does not
//!   fail, it hangs, so a non-interactive stdin is turned away at the
//!   door with the non-interactive commands named instead.
//! - **Nothing is written before it is checked.** A relay address is stored
//!   after a real round trip through it, or after the user says to keep it
//!   despite a failed one -- never silently.
//!
//! The conversation is a function of a scripted reader and a set of actions,
//! so the whole flow is exercised in tests without a terminal, a relay or a
//! login item. Only `run` touches the real ones.

use crate::output::{Reporter, Tone};
use crate::prompt::Console;
use clift_core::error::{CliftError, ErrorKind, Remedy, Stage};
use clift_core::hotkey::Hotkey;
use clift_core::places::Platform;
use clift_core::universal::{DEFAULT_MAX_OBJECT_BYTES, DEFAULT_TTL, RelaySettings};
use std::io::IsTerminal;
use std::path::PathBuf;
use std::time::Duration;

/// The button on the Worker's README, for people with no machine to run a
/// relay on.
const DEPLOY_URL: &str = "https://deploy.workers.cloudflare.com/?url=https://github.com/leazoot/clift/tree/main/relay/cloudflare";

/// The installer every server needs to run once. `--no-setup`, because the
/// questions this conversation asks are about the machine one pastes *from*;
/// a server only needs to know the relay, and that is the line after it.
const SERVER_INSTALL: &str = "curl -fsSL https://raw.githubusercontent.com/leazoot/clift/main/install.sh | sh -s -- --no-setup";

/// Why a relay could not be used, in the words `doctor` would use.
#[derive(Debug)]
pub struct RelayFailure {
    pub detail: String,
    pub remedy: Option<Remedy>,
}

/// Everything the conversation does to the outside world.
///
/// One implementation talks to the real relay, configuration file and login
/// items; the tests script another. The conversation cannot tell them apart,
/// which is the point.
pub trait Actions {
    fn platform(&self) -> Platform;
    /// The relay Clift would use right now, from the configuration or the
    /// environment, if there is one.
    fn configured_relay(&self) -> Option<String>;
    /// Seals, stores and retrieves a small object through the relay at `url`;
    /// the time that took, or why it did not work.
    fn probe_relay(&self, url: &str) -> Result<Duration, RelayFailure>;
    /// Writes `relay.url`; where it was written.
    fn save_relay(&self, url: &str) -> Result<PathBuf, CliftError>;
    /// The combination `clift hotkey` would register right now: the
    /// configured one, else the platform default.
    fn hotkey_combination(&self) -> String;
    /// Writes `hotkey.combination`; where it was written.
    fn save_hotkey(&self, combination: &str) -> Result<PathBuf, CliftError>;
    /// Whether this platform can start the hotkey helper at login.
    fn login_item_supported(&self) -> bool;
    /// Registers the helper to start at login, and starts it now.
    fn install_hotkey(&self) -> Result<(), CliftError>;
    /// The Fast Mode flow for one host, confirmation and all.
    fn configure_host(&self, ssh_host: &str) -> Result<(), CliftError>;
}

/// Runs the conversation on the real terminal.
///
/// # Errors
/// Refuses without an interactive terminal, and under `--json`, naming the
/// commands that do the same work without asking. Otherwise propagates the
/// failure of whichever step the user chose not to skip.
pub fn run(reporter: &Reporter) -> Result<(), CliftError> {
    if reporter.json() {
        return Err(not_interactive(
            "clift setup without a host is a conversation, and --json is for machines",
        ));
    }
    if !std::io::stdin().is_terminal() || !std::io::stderr().is_terminal() {
        return Err(not_interactive(
            "clift setup without a host asks questions, and this is not an interactive terminal",
        ));
    }

    let stdin = std::io::stdin();
    let mut input = stdin.lock();
    let mut output = std::io::stderr();
    let console = Console::new(&mut input, &mut output);
    let live = Live { reporter };
    Wizard {
        console,
        actions: &live,
        reporter,
    }
    .run()
}

fn not_interactive(message: &str) -> CliftError {
    CliftError::new(Stage::Config, ErrorKind::Config, message).with_remedy(Remedy::new(
        "Configure without questions instead (or run `clift setup <ssh-host> --yes` for Fast Mode):",
        "clift config set relay.url https://relay.example.com",
    ))
}

/// The conversation itself.
pub struct Wizard<'a> {
    pub console: Console<'a>,
    pub actions: &'a dyn Actions,
    pub reporter: &'a Reporter,
}

/// Where the relay step ended up.
enum RelayOutcome {
    Configured(String),
    Skipped,
}

impl Wizard<'_> {
    /// # Errors
    /// Propagates the failure of a step the user did not skip.
    pub fn run(mut self) -> Result<(), CliftError> {
        let heading = self.reporter.paint(
            &format!("Clift {}: first-time setup", env!("CARGO_PKG_VERSION")),
            Tone::Heading,
        );
        self.console.say(&heading);
        self.console
            .say("A few questions. Nothing is written until it has been checked.");
        self.console.say("");

        let role = self.console.choose(
            "What is this machine?",
            &[
                "The one I paste from, into any SSH session, in any terminal, through a relay (Universal Mode, recommended)"
                    .to_string(),
                "The one I paste from, into one SSH host, where the file lands directly (Fast Mode)"
                    .to_string(),
                "A server: agents run here and receive what I paste".to_string(),
            ],
            0,
        )?;

        match role {
            1 => return self.fast_mode(),
            2 => return self.server(),
            _ => {}
        }

        self.console.say("");
        let relay = self.relay()?;
        self.console.say("");

        let mut registered = None;
        if let RelayOutcome::Configured(url) = &relay {
            registered = self.hotkey()?;
            self.console.say("");
            self.servers(url);
        } else {
            self.console
                .say("Without a relay, Universal Mode is off until you set one:");
            self.console.say(&format!(
                "  {}",
                self.reporter.paint(
                    "clift config set relay.url https://relay.example.com",
                    Tone::Command
                )
            ));
        }

        self.console.say("");
        let done = self.reporter.paint("Done.", Tone::Ok);
        match registered {
            Some(combination) => {
                self.console.say(&format!(
                    "{done} Copy a screenshot, then press {} in the terminal that is talking to your server.",
                    self.reporter.paint(&combination, Tone::Command)
                ));
                self.console
                    .say("The fetch line appears there a few seconds later; the agent runs it.");
            }
            None => {
                self.console.say(&format!(
                    "{done} Copy a screenshot, then in the terminal that is talking to your server:"
                ));
                self.console.say(&format!(
                    "  {}",
                    self.reporter.paint("clift paste --copy", Tone::Command)
                ));
                self.console.say("and paste.");
            }
        }
        self.console
            .say("`clift doctor` shows all of this again any time.");
        Ok(())
    }

    /// The receiving end: the relay's address and nothing else. A server has
    /// no clipboard to read and no key to press; it only has to know where to
    /// redeem the tokens pasted into it.
    fn server(mut self) -> Result<(), CliftError> {
        self.console.say("");
        self.console.say(
            "A server only needs the relay's address: the one the machine you paste from uses.",
        );
        self.console
            .say("`clift status` there shows it. A token never carries it.");
        match self.relay()? {
            RelayOutcome::Configured(_) => {
                self.console.say("");
                let done = self.reporter.paint("Done.", Tone::Ok);
                self.console.say(&format!(
                    "{done} A token pasted into a session on this machine can be redeemed here with the command that comes with it."
                ));
                self.console.say(
                    "To have the agent handle a failed fetch the way you would, append integrations/agents/clift.md to its instructions file.",
                );
            }
            RelayOutcome::Skipped => {
                self.console.say("");
                self.console
                    .say("Without the relay's address, tokens cannot be redeemed here. Later:");
                self.console.say(&format!(
                    "  {}",
                    self.reporter.paint(
                        "clift config set relay.url <relay-url-from-the-sender>",
                        Tone::Command
                    )
                ));
            }
        }
        Ok(())
    }

    fn fast_mode(mut self) -> Result<(), CliftError> {
        self.console.say("");
        self.console
            .say("Fast Mode uses a host from your ~/.ssh/config, with your own keys and settings.");
        let host = self.console.ask("SSH host alias:")?;
        if host.is_empty() {
            self.console
                .say("No host given. When you have one, run: clift setup <ssh-host>");
            return Ok(());
        }
        self.console.say("");
        self.actions.configure_host(&host)
    }

    fn relay(&mut self) -> Result<RelayOutcome, CliftError> {
        let mut candidate = self.actions.configured_relay();
        if let Some(existing) = &candidate {
            self.console
                .say(&format!("A relay is already configured: {existing}"));
            if !self.console.confirm("Keep it?", true)? {
                candidate = None;
            }
        }

        if candidate.is_none() {
            self.console
                .say("A relay holds the sealed attachment for a few minutes. It cannot read it.");
            let source = self.console.choose(
                "Do you have one?",
                &[
                    "Yes, I have its address".to_string(),
                    "No: deploy one to a free Cloudflare account".to_string(),
                    "No: run clift-relayd on a machine of my own".to_string(),
                ],
                0,
            )?;
            match source {
                1 => {
                    self.console.say("");
                    self.console
                        .say("Open this in a browser, sign in, and press Deploy:");
                    self.console.say(&format!(
                        "  {}",
                        self.reporter.paint(DEPLOY_URL, Tone::Command)
                    ));
                    self.console
                        .say("It ends with an address like https://clift-relay.<you>.workers.dev");
                    self.console.say("");
                }
                2 => {
                    self.console.say("");
                    self.console.say(
                        "Run clift-relayd on a machine every end can reach, behind a TLS proxy;",
                    );
                    self.console.say(
                        "relay/README.md in the repository shows how. The address is the proxy's.",
                    );
                    self.console.say("");
                }
                _ => {}
            }
        }

        loop {
            let url = match candidate.take() {
                Some(url) => url,
                None => {
                    let typed = self.console.ask("Relay address (Enter to skip for now):")?;
                    if typed.is_empty() {
                        self.console.say("Skipped.");
                        return Ok(RelayOutcome::Skipped);
                    }
                    typed
                }
            };

            let settings = match RelaySettings::new(&url, DEFAULT_MAX_OBJECT_BYTES, DEFAULT_TTL) {
                Ok(settings) => settings,
                Err(error) => {
                    self.console.say(&format!(
                        "{} {}",
                        self.reporter.paint("✗", Tone::Fail),
                        error.message()
                    ));
                    continue;
                }
            };
            let url = settings.url().to_string();

            match self.actions.probe_relay(&url) {
                Ok(elapsed) => {
                    self.console.say(&format!(
                        "{} Relay works: sealed, stored and retrieved a test object in {}",
                        self.reporter.paint("✓", Tone::Ok),
                        took(elapsed)
                    ));
                    let path = self.actions.save_relay(&url)?;
                    self.console
                        .say(&format!("  saved relay.url in {}", path.display()));
                    return Ok(RelayOutcome::Configured(url));
                }
                Err(failure) => {
                    self.console.say(&format!(
                        "{} {}",
                        self.reporter.paint("✗", Tone::Fail),
                        failure.detail
                    ));
                    if let Some(remedy) = &failure.remedy {
                        self.console.say(&format!("  {}", remedy.description()));
                        self.console.say(&format!(
                            "    {}",
                            self.reporter.paint(remedy.command(), Tone::Command)
                        ));
                    }
                    if looks_like_no_route(&failure.detail) {
                        self.proxy_hint();
                    }
                    let next = self.console.choose(
                        "What now?",
                        &[
                            "Try again".to_string(),
                            "Save the address anyway".to_string(),
                            "Enter a different address".to_string(),
                            "Skip for now".to_string(),
                        ],
                        0,
                    )?;
                    match next {
                        0 => candidate = Some(url),
                        1 => {
                            let path = self.actions.save_relay(&url)?;
                            self.console
                                .say(&format!("  saved relay.url in {}", path.display()));
                            return Ok(RelayOutcome::Configured(url));
                        }
                        2 => {}
                        _ => {
                            self.console.say("Skipped.");
                            return Ok(RelayOutcome::Skipped);
                        }
                    }
                }
            }
        }
    }

    /// Clift sees a proxy only through `HTTPS_PROXY`, like curl. A browser that
    /// works while Clift is refused is the usual sign.
    fn proxy_hint(&mut self) {
        let example = match self.actions.platform() {
            Platform::Windows => "$env:HTTPS_PROXY = \"http://127.0.0.1:7890\"",
            Platform::Unix => "export HTTPS_PROXY=http://127.0.0.1:7890",
        };
        self.console.say(
            "  If this machine reaches the internet through a proxy, Clift only sees it through HTTPS_PROXY, for example:",
        );
        self.console.say(&format!(
            "    {}",
            self.reporter.paint(example, Tone::Command)
        ));
    }

    /// The key, then the login entry. Returns the combination when the
    /// helper was registered, so the closing lines can name it.
    ///
    /// The combination is asked for before the registration because the
    /// login entry reads it from the file: the order in which the two are
    /// written is the order in which they take effect.
    fn hotkey(&mut self) -> Result<Option<String>, CliftError> {
        self.console.say(
            "One key combination sends the screenshot and pastes the fetch line into whatever window has focus.",
        );
        let combination = self.combination()?;

        if !self.actions.login_item_supported() {
            self.console.say(&format!(
                "It runs as `clift hotkey` in a terminal you keep open ({combination});"
            ));
            self.console
                .say("starting it at login is not available on this platform yet.");
            return Ok(None);
        }
        if !self.console.confirm(
            &format!("Start {combination} at login, and now? No terminal needs to stay open."),
            true,
        )? {
            self.console.say("Skipped. Later: clift hotkey --install");
            return Ok(None);
        }
        self.actions.install_hotkey()?;
        match self.actions.platform() {
            Platform::Windows => {
                self.console.say(&format!(
                    "{} It is running now, hidden; there is no window to close. Stop it any time with: clift hotkey --uninstall",
                    self.reporter.paint("Note:", Tone::Warn)
                ));
            }
            Platform::Unix => {
                self.console.say(&format!(
                    "{} macOS will now ask whether clift may control this computer: allow it, that is what lets the key paste for you.",
                    self.reporter.paint("Note:", Tone::Warn)
                ));
                self.console.say(
                    "  Until it is allowed, each press only copies the text; paste it yourself with Cmd+V.",
                );
            }
        }
        Ok(Some(combination))
    }

    /// Enter keeps what is configured (or the platform default); anything
    /// typed is checked by the same rules `clift config set` applies, and a
    /// refused answer is asked again rather than replaced by the default.
    fn combination(&mut self) -> Result<String, CliftError> {
        let current = self.actions.hotkey_combination();
        loop {
            let typed = self.console.ask(&format!("Key combination [{current}]:"))?;
            if typed.is_empty() {
                return Ok(current);
            }
            let hotkey = match Hotkey::parse(&typed) {
                Ok(hotkey) => hotkey,
                Err(error) => {
                    self.console.say(&format!(
                        "{} {}",
                        self.reporter.paint("✗", Tone::Fail),
                        error
                    ));
                    self.console.say(
                        "  Modifiers are ctrl, alt, shift and cmd (the Windows key on Windows); the key is a letter, a digit or F1 to F12. For example: ctrl+alt+v",
                    );
                    continue;
                }
            };
            let rendered = hotkey.render();
            if let Some(warning) = hotkey.warning() {
                self.console.say(&format!(
                    "{} {warning}",
                    self.reporter.paint("Note:", Tone::Warn)
                ));
            }
            if rendered != current {
                let path = self.actions.save_hotkey(&rendered)?;
                self.console
                    .say(&format!("  saved hotkey.combination in {}", path.display()));
            }
            return Ok(rendered);
        }
    }

    fn servers(&mut self, url: &str) {
        self.console.say(
            &self
                .reporter
                .paint("On each server you paste into, once:", Tone::Heading),
        );
        self.console.say(&format!(
            "  {}",
            self.reporter.paint(SERVER_INSTALL, Tone::Command)
        ));
        self.console.say(&format!(
            "  {}",
            self.reporter
                .paint(&format!("clift config set relay.url {url}"), Tone::Command)
        ));
        self.console.say(
            "A token carries the object and its key, never the relay's address, so every server needs it.",
        );
        self.console.say(
            "To have the agent handle a failed fetch the way you would, append integrations/agents/clift.md to its instructions file.",
        );
    }
}

/// A duration the way a person reads one: milliseconds under a second, so a
/// relay on the same machine does not report "0.0s".
fn took(elapsed: Duration) -> String {
    if elapsed < Duration::from_secs(1) {
        format!("{} ms", elapsed.as_millis())
    } else {
        format!("{:.1} s", elapsed.as_secs_f64())
    }
}

/// The failures a proxy or a poisoned DNS answer produce, as the relay client
/// words them. Anything else (a 404, a bad document) is the relay's problem,
/// not the route to it.
fn looks_like_no_route(detail: &str) -> bool {
    let detail = detail.to_ascii_lowercase();
    [
        "could not be reached",
        "refused",
        "timed out",
        "resolve",
        "dns",
    ]
    .iter()
    .any(|sign| detail.contains(sign))
}

/// The real world.
struct Live<'a> {
    reporter: &'a Reporter,
}

impl Actions for Live<'_> {
    fn platform(&self) -> Platform {
        Platform::current()
    }

    fn configured_relay(&self) -> Option<String> {
        let path = clift_core::config::io::default_config_path().ok()?;
        let loaded = clift_core::config::io::load(&path).ok()?;
        crate::relay::is_configured(&loaded.config)
            .then(|| crate::relay::settings(&loaded.config).ok())
            .flatten()
            .map(|settings| settings.url().to_string())
    }

    fn probe_relay(&self, url: &str) -> Result<Duration, RelayFailure> {
        use crate::progress::Spinner;
        use crate::system::SystemRandomness;
        use clift_core::diagnostics::{RelayProbe, probe_relay};
        use clift_core::ports::CheckStatus;

        let settings =
            RelaySettings::new(url, DEFAULT_MAX_OBJECT_BYTES, DEFAULT_TTL).map_err(|error| {
                RelayFailure {
                    detail: error.message().to_string(),
                    remedy: None,
                }
            })?;
        let client = clift_relay::HttpRelay::new(&settings);
        let spinner = Spinner::new(self.reporter.interactive());
        spinner.begin(format!("Checking the relay at {url}"));
        let started = std::time::Instant::now();
        let check = probe_relay(&RelayProbe {
            settings: &settings,
            relay: &client,
            random: &SystemRandomness,
        });
        let elapsed = started.elapsed();
        drop(spinner);

        match check.status {
            CheckStatus::Pass => Ok(elapsed),
            _ => Err(RelayFailure {
                detail: check.detail,
                remedy: check.remedy,
            }),
        }
    }

    fn save_relay(&self, url: &str) -> Result<PathBuf, CliftError> {
        let path = clift_core::config::io::default_config_path()?;
        super::config::write_key(&path, "relay.url", url, self.reporter)?;
        Ok(path)
    }

    fn hotkey_combination(&self) -> String {
        super::hotkey::combination(None, self.reporter).map_or_else(
            |_| clift_core::hotkey::default_combination().render(),
            |hotkey| hotkey.render(),
        )
    }

    fn save_hotkey(&self, combination: &str) -> Result<PathBuf, CliftError> {
        let path = clift_core::config::io::default_config_path()?;
        super::config::write_key(&path, "hotkey.combination", combination, self.reporter)?;
        Ok(path)
    }

    fn login_item_supported(&self) -> bool {
        clift_inject::autostart::is_supported()
    }

    fn install_hotkey(&self) -> Result<(), CliftError> {
        super::hotkey::run(None, true, false, self.reporter)
    }

    fn configure_host(&self, ssh_host: &str) -> Result<(), CliftError> {
        super::setup::configure_host(ssh_host, false, self.reporter)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::io::Cursor;

    /// Answers every action from a script and records what was asked of it.
    struct Scripted {
        platform: Platform,
        configured: Option<String>,
        probe: Vec<Result<Duration, RelayFailure>>,
        login_item: bool,
        saved: RefCell<Vec<String>>,
        saved_hotkeys: RefCell<Vec<String>>,
        hotkey_installed: RefCell<bool>,
        hosts: RefCell<Vec<String>>,
        probed: RefCell<Vec<String>>,
    }

    impl Scripted {
        fn new() -> Self {
            Self {
                platform: Platform::Unix,
                configured: None,
                probe: Vec::new(),
                login_item: true,
                saved: RefCell::new(Vec::new()),
                saved_hotkeys: RefCell::new(Vec::new()),
                hotkey_installed: RefCell::new(false),
                hosts: RefCell::new(Vec::new()),
                probed: RefCell::new(Vec::new()),
            }
        }
    }

    impl Actions for Scripted {
        fn platform(&self) -> Platform {
            self.platform
        }
        fn configured_relay(&self) -> Option<String> {
            self.configured.clone()
        }
        fn probe_relay(&self, url: &str) -> Result<Duration, RelayFailure> {
            self.probed.borrow_mut().push(url.to_string());
            let index = self.probed.borrow().len() - 1;
            match self.probe.get(index) {
                Some(Ok(elapsed)) => Ok(*elapsed),
                Some(Err(failure)) => Err(RelayFailure {
                    detail: failure.detail.clone(),
                    remedy: failure.remedy.clone(),
                }),
                None => panic!("unexpected probe of {url}"),
            }
        }
        fn save_relay(&self, url: &str) -> Result<PathBuf, CliftError> {
            self.saved.borrow_mut().push(url.to_string());
            Ok(PathBuf::from("/home/dev/.config/clift/config.toml"))
        }
        fn hotkey_combination(&self) -> String {
            match self.platform {
                Platform::Windows => "ctrl+alt+v".to_string(),
                Platform::Unix => "cmd+shift+v".to_string(),
            }
        }
        fn save_hotkey(&self, combination: &str) -> Result<PathBuf, CliftError> {
            self.saved_hotkeys
                .borrow_mut()
                .push(combination.to_string());
            Ok(PathBuf::from("/home/dev/.config/clift/config.toml"))
        }
        fn login_item_supported(&self) -> bool {
            self.login_item
        }
        fn install_hotkey(&self) -> Result<(), CliftError> {
            *self.hotkey_installed.borrow_mut() = true;
            Ok(())
        }
        fn configure_host(&self, ssh_host: &str) -> Result<(), CliftError> {
            self.hosts.borrow_mut().push(ssh_host.to_string());
            Ok(())
        }
    }

    fn converse(answers: &str, actions: &Scripted) -> (Result<(), CliftError>, String) {
        let reporter = Reporter::new(false, false, false);
        let mut input = Cursor::new(answers.as_bytes().to_vec());
        let mut output = Vec::new();
        let outcome = Wizard {
            console: Console::new(&mut input, &mut output),
            actions,
            reporter: &reporter,
        }
        .run();
        (outcome, String::from_utf8(output).unwrap())
    }

    fn refused(detail: &str) -> RelayFailure {
        RelayFailure {
            detail: detail.to_string(),
            remedy: Some(Remedy::new(
                "Check the relay is up:",
                "curl -sS x/v1/health",
            )),
        }
    }

    #[test]
    fn the_happy_path_checks_the_relay_before_saving_it_and_then_explains_the_servers() {
        let mut actions = Scripted::new();
        actions.probe = vec![Ok(Duration::from_millis(800))];
        // Universal, have an address, the address, keep the default key,
        // register the hotkey.
        let (outcome, shown) = converse("1\n1\nhttps://relay.example/\n\ny\n", &actions);
        outcome.unwrap();

        assert_eq!(
            actions.probed.borrow().as_slice(),
            ["https://relay.example"]
        );
        assert_eq!(actions.saved.borrow().as_slice(), ["https://relay.example"]);
        assert!(*actions.hotkey_installed.borrow());
        let probe_at = shown.find("Relay works").unwrap();
        let saved_at = shown.find("saved relay.url").unwrap();
        assert!(probe_at < saved_at, "checked before saved: {shown}");
        assert!(
            shown.contains("clift config set relay.url https://relay.example"),
            "{shown}"
        );
        assert!(
            actions.saved_hotkeys.borrow().is_empty(),
            "Enter keeps the default: {shown}"
        );
        assert!(
            shown.contains("then press cmd+shift+v in the terminal"),
            "the closing line names the key, not --copy: {shown}"
        );
        assert!(!shown.contains("clift paste --copy"), "{shown}");
    }

    #[test]
    fn a_relay_that_fails_is_not_saved_unless_the_user_says_so() {
        let mut actions = Scripted::new();
        actions.probe = vec![Err(refused(
            "the relay at x could not be reached: io: Connection refused",
        ))];
        // Universal, have an address, the address, then "skip for now".
        let (outcome, shown) = converse("1\n1\nhttps://relay.example\n4\n", &actions);
        outcome.unwrap();
        assert!(actions.saved.borrow().is_empty(), "{shown}");
        assert!(shown.contains("Connection refused"), "{shown}");
        assert!(shown.contains("curl -sS x/v1/health"), "{shown}");
        assert!(
            shown.contains("export HTTPS_PROXY="),
            "the proxy hint: {shown}"
        );
        assert!(
            shown.contains("Universal Mode is off until you set one"),
            "{shown}"
        );
    }

    #[test]
    fn save_anyway_is_an_explicit_choice_and_try_again_probes_again() {
        let mut actions = Scripted::new();
        actions.probe = vec![Err(refused("timed out")), Err(refused("timed out"))];
        // Try again once, then save anyway, keep the key, decline the login entry.
        let (outcome, shown) = converse("1\n1\nhttps://relay.example\n1\n2\n\nn\n", &actions);
        outcome.unwrap();
        assert_eq!(actions.probed.borrow().len(), 2);
        assert_eq!(actions.saved.borrow().as_slice(), ["https://relay.example"]);
        assert!(!*actions.hotkey_installed.borrow());
        assert!(shown.contains("clift hotkey --install"), "{shown}");
        assert!(
            shown.contains("clift paste --copy"),
            "without the key, the closing line falls back to --copy: {shown}"
        );
    }

    #[test]
    fn a_bad_address_is_refused_before_any_probe() {
        let mut actions = Scripted::new();
        actions.probe = vec![Ok(Duration::from_millis(100))];
        let (outcome, shown) = converse(
            "1\n1\nrelay.example\nhttps://relay.example\n\ny\n",
            &actions,
        );
        outcome.unwrap();
        assert_eq!(actions.probed.borrow().len(), 1, "{shown}");
        assert!(
            shown.contains("must start with http:// or https://"),
            "{shown}"
        );
    }

    #[test]
    fn on_windows_the_hint_is_a_powershell_line_and_the_helper_starts_hidden_at_login() {
        let mut actions = Scripted::new();
        actions.platform = Platform::Windows;
        actions.probe = vec![
            Err(refused("could not be reached: dns")),
            Ok(Duration::from_secs(1)),
        ];
        let (outcome, shown) = converse(
            "1\n1\nhttps://a.example\n3\nhttps://b.example\n\ny\n",
            &actions,
        );
        outcome.unwrap();
        assert!(shown.contains("$env:HTTPS_PROXY"), "{shown}");
        assert!(!shown.contains("export HTTPS_PROXY"), "{shown}");
        assert_eq!(actions.saved.borrow().as_slice(), ["https://b.example"]);
        assert!(shown.contains("Key combination [ctrl+alt+v]:"), "{shown}");
        assert!(*actions.hotkey_installed.borrow());
        assert!(shown.contains("there is no window to close"), "{shown}");
        assert!(!shown.contains("macOS will now ask"), "{shown}");
        assert!(shown.contains("then press ctrl+alt+v"), "{shown}");
    }

    #[test]
    fn where_login_items_do_not_exist_the_key_is_named_and_nothing_is_promised() {
        let mut actions = Scripted::new();
        actions.login_item = false;
        actions.probe = vec![Ok(Duration::from_secs(1))];
        let (outcome, shown) = converse("1\n1\nhttps://a.example\n\n", &actions);
        outcome.unwrap();
        assert!(
            shown.contains("not available on this platform yet"),
            "{shown}"
        );
        assert!(!*actions.hotkey_installed.borrow());
        assert!(shown.contains("clift paste --copy"), "{shown}");
    }

    #[test]
    fn a_typed_combination_is_saved_in_its_canonical_spelling_and_then_registered() {
        let mut actions = Scripted::new();
        actions.probe = vec![Ok(Duration::from_millis(100))];
        let (outcome, shown) = converse(
            "1\n1\nhttps://relay.example\nSHIFT + Cmd + F9\ny\n",
            &actions,
        );
        outcome.unwrap();
        assert_eq!(actions.saved_hotkeys.borrow().as_slice(), ["cmd+shift+f9"]);
        let saved_at = shown.find("saved hotkey.combination").unwrap();
        let asked_at = shown.find("Start cmd+shift+f9 at login").unwrap();
        assert!(saved_at < asked_at, "saved before registered: {shown}");
        assert!(*actions.hotkey_installed.borrow());
        assert!(shown.contains("then press cmd+shift+f9"), "{shown}");
    }

    #[test]
    fn a_combination_clift_refuses_is_asked_again_not_replaced_by_the_default() {
        let mut actions = Scripted::new();
        actions.probe = vec![Ok(Duration::from_millis(100))];
        // The plain paste key, then no modifier, then something acceptable.
        let (outcome, shown) = converse(
            "1\n1\nhttps://relay.example\ncmd+v\nv\nctrl+alt+a\ny\n",
            &actions,
        );
        outcome.unwrap();
        assert!(shown.contains("ordinary paste key"), "{shown}");
        assert!(shown.contains("without a modifier"), "{shown}");
        assert_eq!(
            shown.matches("Key combination [cmd+shift+v]:").count(),
            3,
            "{shown}"
        );
        assert_eq!(actions.saved_hotkeys.borrow().as_slice(), ["ctrl+alt+a"]);
    }

    #[test]
    fn the_terminal_paste_key_is_accepted_with_a_warning() {
        let mut actions = Scripted::new();
        actions.platform = Platform::Windows;
        actions.probe = vec![Ok(Duration::from_millis(100))];
        let (outcome, shown) = converse("1\n1\nhttps://relay.example\nctrl+shift+v\ny\n", &actions);
        outcome.unwrap();
        assert!(shown.contains("paste key in most terminals"), "{shown}");
        assert_eq!(actions.saved_hotkeys.borrow().as_slice(), ["ctrl+shift+v"]);
        assert!(*actions.hotkey_installed.borrow());
    }

    #[test]
    fn an_existing_relay_is_offered_and_kept_without_retyping() {
        let mut actions = Scripted::new();
        actions.configured = Some("https://kept.example".to_string());
        actions.probe = vec![Ok(Duration::from_millis(300))];
        let (outcome, shown) = converse("1\n\n\nn\n", &actions);
        outcome.unwrap();
        assert!(
            shown.contains("already configured: https://kept.example"),
            "{shown}"
        );
        assert_eq!(actions.probed.borrow().as_slice(), ["https://kept.example"]);
    }

    #[test]
    fn deploy_and_self_host_paths_print_where_to_go_and_accept_skipping() {
        let actions = Scripted::new();
        let (outcome, shown) = converse("1\n2\n\n", &actions);
        outcome.unwrap();
        assert!(shown.contains(DEPLOY_URL), "{shown}");
        assert!(shown.contains("Skipped."), "{shown}");
        assert!(actions.probed.borrow().is_empty());

        let (outcome, shown) = converse("1\n3\n\n", &actions);
        outcome.unwrap();
        assert!(shown.contains("clift-relayd"), "{shown}");
    }

    #[test]
    fn a_server_only_asks_for_the_relay_and_never_offers_the_hotkey() {
        let mut actions = Scripted::new();
        actions.probe = vec![Ok(Duration::from_millis(200))];
        let (outcome, shown) = converse("3\n1\nhttps://relay.example\n", &actions);
        outcome.unwrap();
        assert_eq!(actions.saved.borrow().as_slice(), ["https://relay.example"]);
        assert!(!*actions.hotkey_installed.borrow());
        assert!(!shown.contains("starts at login"), "{shown}");
        assert!(shown.contains("redeemed here"), "{shown}");

        let (outcome, shown) = converse("3\n1\n\n", &actions);
        outcome.unwrap();
        assert!(shown.contains("relay-url-from-the-sender"), "{shown}");
    }

    #[test]
    fn the_server_instructions_skip_the_conversation_on_the_server() {
        assert!(SERVER_INSTALL.ends_with("--no-setup"), "{SERVER_INSTALL}");
    }

    #[test]
    fn granting_the_hotkey_says_what_macos_will_ask_and_what_happens_until_then() {
        let mut actions = Scripted::new();
        actions.probe = vec![Ok(Duration::from_millis(100))];
        let (outcome, shown) = converse("1\n1\nhttps://relay.example\n\ny\n", &actions);
        outcome.unwrap();
        assert!(shown.contains("allow it"), "{shown}");
        assert!(shown.contains("paste it yourself with Cmd+V"), "{shown}");
    }

    #[test]
    fn fast_mode_hands_the_host_to_the_existing_flow() {
        let actions = Scripted::new();
        let (outcome, shown) = converse("2\ncore\n", &actions);
        outcome.unwrap();
        assert_eq!(actions.hosts.borrow().as_slice(), ["core"]);
        assert!(actions.probed.borrow().is_empty(), "{shown}");

        let (outcome, shown) = converse("2\n\n", &actions);
        outcome.unwrap();
        assert!(shown.contains("clift setup <ssh-host>"), "{shown}");
    }

    #[test]
    fn input_that_ends_mid_conversation_is_an_error_and_writes_nothing() {
        let actions = Scripted::new();
        let (outcome, _) = converse("1\n1\n", &actions);
        assert!(outcome.is_err());
        assert!(actions.saved.borrow().is_empty());
    }

    #[test]
    fn a_duration_reads_as_milliseconds_under_a_second() {
        assert_eq!(took(Duration::from_millis(12)), "12 ms");
        assert_eq!(took(Duration::from_millis(1850)), "1.9 s");
    }

    #[test]
    fn only_route_failures_get_the_proxy_hint() {
        assert!(looks_like_no_route(
            "the relay at x could not be reached: io: Connection refused"
        ));
        assert!(looks_like_no_route("Timed Out"));
        assert!(!looks_like_no_route(
            "the relay answered 404 to the health check"
        ));
        assert!(!looks_like_no_route("the health document is not JSON"));
    }
}
