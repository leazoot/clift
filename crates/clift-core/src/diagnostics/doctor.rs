//! The thirteen checks of the specification.
//!
//! Two rules shape this module. **No check stops another**: a user with three
//! problems should learn about all three in one run, not discover them one
//! command at a time. And **every failure carries exactly one command**: a list
//! of things to try is a list of decisions the user now has to make, which is
//! the opposite of help.
//!
//! Checks that depend on machinery Clift does not have yet report `Warn` and
//! say so in as many words. Reporting `Pass` for something that was never
//! examined would be a lie, and silently skipping it would leave a gap the user
//! cannot see.

use crate::error::Remedy;
use crate::ports::{
    CheckStatus, ClipboardSource, Randomness, Relay, RemoteFs, RemoteUpload, SshConfigSource,
    TransportTarget,
};
use crate::staging::{ensure_inbox, verify_round_trip};
use crate::universal::RelaySettings;
use crate::universal::crypto::{self, NONCE_BYTES};
use crate::universal::token::{SEAL_KEY_BYTES, SealKey};

/// Every check `doctor` performs, in the order it performs them.
///
/// An enum rather than strings so that adding, removing or renaming one is a
/// compile-time event: the JSON output and the tests both key on these.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum CheckName {
    Platform,
    Clipboard,
    SshClient,
    SftpClient,
    HostResolution,
    Authentication,
    SftpSubsystem,
    RemoteHome,
    InboxPermissions,
    UploadAndCleanup,
    /// Can this machine type the instruction itself, and was the
    /// permission granted to the binary that would need it?
    KeystrokeInjection,
    /// Universal Mode's end of things: is a relay configured, and does a small
    /// object actually go there and come back?
    Relay,
    ConfigVersion,
}

impl CheckName {
    /// Every check, in report order.
    pub const ALL: [CheckName; 13] = [
        CheckName::Platform,
        CheckName::Clipboard,
        CheckName::SshClient,
        CheckName::SftpClient,
        CheckName::HostResolution,
        CheckName::Authentication,
        CheckName::SftpSubsystem,
        CheckName::RemoteHome,
        CheckName::InboxPermissions,
        CheckName::UploadAndCleanup,
        CheckName::KeystrokeInjection,
        CheckName::Relay,
        CheckName::ConfigVersion,
    ];

    /// The stable identifier used in `--json` and in the human report.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            CheckName::Platform => "platform",
            CheckName::Clipboard => "clipboard",
            CheckName::SshClient => "ssh client",
            CheckName::SftpClient => "sftp client",
            CheckName::HostResolution => "host resolution",
            CheckName::Authentication => "authentication",
            CheckName::SftpSubsystem => "sftp subsystem",
            CheckName::RemoteHome => "remote home",
            CheckName::InboxPermissions => "inbox permissions",
            CheckName::UploadAndCleanup => "upload and cleanup",
            CheckName::KeystrokeInjection => "keystroke injection",
            CheckName::Relay => "relay",
            CheckName::ConfigVersion => "config version",
        }
    }
}

/// One line of the report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DoctorCheck {
    pub name: CheckName,
    pub status: CheckStatus,
    pub detail: String,
    /// Exactly one command, and only when something is wrong.
    pub remedy: Option<Remedy>,
}

/// The whole report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DoctorReport {
    pub checks: Vec<DoctorCheck>,
}

impl DoctorReport {
    /// True when nothing failed. Warnings do not make a report fail: a machine
    /// with no host configured yet is not a broken installation.
    #[must_use]
    pub fn passed(&self) -> bool {
        self.checks
            .iter()
            .all(|check| check.status != CheckStatus::Fail)
    }

    #[must_use]
    pub fn failures(&self) -> usize {
        self.count(CheckStatus::Fail)
    }

    #[must_use]
    pub fn warnings(&self) -> usize {
        self.count(CheckStatus::Warn)
    }

    fn count(&self, status: CheckStatus) -> usize {
        self.checks
            .iter()
            .filter(|check| check.status == status)
            .count()
    }
}

/// What the running binary knows about itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalFacts {
    pub platform: String,
    pub version: String,
}

/// What was found in `config.toml`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigState {
    pub exists: bool,
    pub version: u32,
    pub supported: u32,
    /// Anything the parser wanted to say, such as an unrecognised key.
    pub warnings: Vec<String>,
    /// Why the file could not be read, when it could not be.
    ///
    /// A configuration Clift cannot parse must not stop `doctor`: a user whose
    /// config is broken is exactly the user who needs the other twelve answers.
    pub error: Option<String>,
}

/// Whether this machine can be asked to synthesise keystrokes.
///
/// Mirrors what the injection adapter found. It is carried as a value rather
/// than read through a port because there is nothing for `doctor` to *perform*
/// here -- the question is answered by asking the operating system once, which
/// the composition root does, exactly as it does for the platform triple.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InjectionState {
    /// The permission is granted. Not a promise about where the keystrokes will
    /// land: that depends on which window has focus.
    Ready,
    /// The mechanism exists but has not been allowed. Carries the one command
    /// that opens the place where a user grants it, because where that is is
    /// platform knowledge and belongs with the platform code. `None` on a
    /// platform that has no such place, where the fall back is the only advice
    /// worth giving.
    NeedsPermission { command: Option<String> },
    /// No implementation on this platform, and why.
    Unsupported { reason: String },
}

/// What `doctor` needs in order to judge the typing path.
///
/// `program` and `helper` are both here because of the way this actually
/// fails. macOS attaches the permission to a *binary*, not to a path or a
/// name, so a user who grants it to one copy of `clift` and registers a
/// different copy to run at login has two things that each look correct on
/// their own and do not work together. Comparing them is the whole point of
/// the check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InjectionFacts {
    pub state: InjectionState,
    /// The binary that is running, which is the one the permission attaches to.
    pub program: Option<String>,
    /// The binary the hotkey helper starts at login, when one is registered.
    pub helper: Option<String>,
}

/// The ports `doctor` inspects the world through.
///
/// The optional ones are the capabilities this build may not have yet. `None`
/// produces a warning that says which milestone brings it, rather than a pass
/// for something that was never tried.
pub struct Environment<'a> {
    pub facts: LocalFacts,
    pub ssh_config: &'a dyn SshConfigSource,
    pub remote: &'a dyn RemoteFs,
    pub upload: &'a dyn RemoteUpload,
    pub clipboard: Option<&'a dyn ClipboardSource>,
    /// The inbox location the configuration asks for, when it asks for one.
    pub remote_dir: Option<String>,
    /// What the injection adapter reports about this machine, when this build
    /// has one.
    pub injection: Option<InjectionFacts>,
    /// Universal Mode's relay, when one is configured.
    pub relay: Option<RelayProbe<'a>>,
    pub config: ConfigState,
}

/// Everything the relay check needs to do a real round trip.
///
/// Carried as a bundle because the check is not "can we reach a URL" -- it is
/// "can this machine seal something, leave it with that relay, and get it back
/// unaltered", which needs the settings, the client and a random source
/// together. A reachability ping would pass on a relay that stores nothing.
pub struct RelayProbe<'a> {
    pub settings: &'a RelaySettings,
    pub relay: &'a dyn Relay,
    pub random: &'a dyn Randomness,
}

/// Runs every check, in order, without letting one stop the next.
///
/// `target` is `None` when the user has nothing configured yet; the checks that
/// need a host then report what to do about that instead of failing.
#[must_use]
pub fn diagnose(environment: &Environment<'_>, target: Option<&TransportTarget>) -> DoctorReport {
    let mut checks = Vec::with_capacity(CheckName::ALL.len());

    checks.push(pass(
        CheckName::Platform,
        format!(
            "clift {} on {}",
            environment.facts.version, environment.facts.platform
        ),
    ));
    checks.push(clipboard_check(environment));

    let Some(target) = target else {
        for name in [
            CheckName::SshClient,
            CheckName::SftpClient,
            CheckName::HostResolution,
            CheckName::Authentication,
            CheckName::SftpSubsystem,
            CheckName::RemoteHome,
            CheckName::InboxPermissions,
            CheckName::UploadAndCleanup,
        ] {
            checks.push(no_fast_mode_host(name, environment.relay.is_some()));
        }
        checks.push(injection_check(environment));
        checks.push(relay_check(environment));
        checks.push(config_check(environment));
        return DoctorReport { checks };
    };

    checks.extend(remote_checks(environment, target));
    checks.push(injection_check(environment));
    checks.push(relay_check(environment));
    checks.push(config_check(environment));

    DoctorReport { checks }
}

/// Can Clift put the attachment in front of the user without a terminal plugin?
///
/// This is the half of Universal Mode that has nothing to do with the relay:
/// `--inject` and the hotkey helper both end in the instruction being typed
/// into the focused window, and on macOS synthesising those keystrokes needs a
/// permission the user grants by hand. Missing
/// it is a warning rather than a failure -- `--copy` is a complete way to use
/// Clift and calling that installation broken would train the user to ignore
/// this report -- but it is never silently skipped, because the
/// symptom otherwise is a key press that appears to do nothing at all.
///
/// The mismatch branch exists because it is the failure that actually
/// happened: the permission was granted to one `clift` binary and the login
/// helper was registered to run another. Both halves looked right in isolation.
fn injection_check(environment: &Environment<'_>) -> DoctorCheck {
    let Some(facts) = &environment.injection else {
        return not_built_in(
            CheckName::KeystrokeInjection,
            "typing into the focused window",
        );
    };

    let fall_back = || {
        Remedy::new(
            "Until then, put the text on the clipboard and paste it yourself:",
            "clift paste --copy",
        )
    };

    match &facts.state {
        InjectionState::Unsupported { reason } => DoctorCheck {
            name: CheckName::KeystrokeInjection,
            status: CheckStatus::Warn,
            detail: format!("not available: {reason}"),
            remedy: Some(fall_back()),
        },
        InjectionState::NeedsPermission { command } => DoctorCheck {
            name: CheckName::KeystrokeInjection,
            status: CheckStatus::Warn,
            detail: format!(
                "{} is not allowed to send keystrokes, so --inject and the hotkey fall back \
                 to --copy",
                facts.program.as_deref().unwrap_or("this binary")
            ),
            remedy: Some(match command {
                Some(command) => Remedy::new(
                    "Allow it to control the computer, then run doctor again:",
                    command.clone(),
                ),
                None => fall_back(),
            }),
        },
        InjectionState::Ready => ready(facts),
    }
}

/// The permission is granted; the remaining question is whether it was granted
/// to the binary that will be doing the pressing.
fn ready(facts: &InjectionFacts) -> DoctorCheck {
    let running = facts.program.as_deref().unwrap_or("this binary");
    match &facts.helper {
        Some(helper) if Some(helper.as_str()) != facts.program.as_deref() => DoctorCheck {
            name: CheckName::KeystrokeInjection,
            status: CheckStatus::Warn,
            detail: format!(
                "can type into the focused window, but the hotkey helper starts {helper} and \
                 this is {running}; permission is granted per binary, so the helper may not \
                 have it"
            ),
            // One command, and it says which way it resolves the
            // disagreement: the other way out is to run the helper's binary
            // instead, and a report that offered both would be handing the
            // user a decision rather than help.
            remedy: Some(Remedy::new(
                "Make them agree -- this registers the one you are running:",
                "clift hotkey --install",
            )),
        },
        Some(helper) => pass(
            CheckName::KeystrokeInjection,
            format!(
                "can type into the focused window; the hotkey helper is registered to start \
                 {helper} at login"
            ),
        ),
        None => pass(
            CheckName::KeystrokeInjection,
            format!(
                "{running} can type into the focused window; no hotkey helper is registered \
                 to start at login"
            ),
        ),
    }
}

/// Publishes a few bytes, fetches them back, and checks they are the same.
///
/// A real round trip rather than a ping, for the same reason
/// [`verify_round_trip`] exists on the SFTP side: a relay that answers its
/// health endpoint and then loses everything you give it would pass any
/// cheaper check. The probe is tiny, sealed like anything else, and consumed
/// by its own retrieval, so it leaves nothing behind.
///
/// No relay configured is a warning, not a failure. Fast Mode is a complete way
/// to use Clift, and telling those users their installation is broken would
/// train them to ignore `doctor`.
fn relay_check(environment: &Environment<'_>) -> DoctorCheck {
    let Some(probe) = &environment.relay else {
        return DoctorCheck {
            name: CheckName::Relay,
            status: CheckStatus::Warn,
            detail: "not checked: no relay is configured, so Universal Mode is unavailable"
                .to_string(),
            remedy: Some(Remedy::new(
                "Point Clift at a relay, or keep using Fast Mode:",
                "clift config set relay.url https://relay.example.com",
            )),
        };
    };
    probe_relay(probe)
}

/// One real round trip through a relay, as a check: seal a tiny payload, leave
/// it there, take it back, compare. Public because the first-time setup asks
/// the same question of an address before it saves it, and two probes that
/// could disagree would be worse than one.
pub fn probe_relay(probe: &RelayProbe<'_>) -> DoctorCheck {
    let url = probe.settings.url().to_string();
    let remedy = || {
        Remedy::new(
            "Check the relay is up:",
            format!("curl -sS {url}/v1/health"),
        )
    };
    let internal = || {
        Remedy::new(
            "Report this with the version, if it happens again:",
            "clift --version",
        )
    };

    const PAYLOAD: &[u8] = b"clift relay self check";
    let mut key_bytes = [0_u8; SEAL_KEY_BYTES];
    let mut nonce = [0_u8; NONCE_BYTES];
    if let Err(error) = probe
        .random
        .fill(&mut key_bytes)
        .and_then(|()| probe.random.fill(&mut nonce))
    {
        return fail(CheckName::Relay, error.message().to_string(), internal());
    }
    let key = SealKey::from_bytes(key_bytes);
    key_bytes.fill(0);

    let sealed = match crypto::seal(PAYLOAD, &key, &nonce) {
        Ok(sealed) => sealed,
        Err(error) => return fail(CheckName::Relay, error.message().to_string(), internal()),
    };
    let published = match probe.relay.publish(&sealed, probe.settings.ttl()) {
        Ok(published) => published,
        Err(error) => return fail(CheckName::Relay, error.message().to_string(), remedy()),
    };
    let returned = match probe.relay.retrieve(&published.id) {
        Ok(bytes) => bytes,
        Err(error) => {
            // Best effort: the object is still there and will expire, but
            // leaving it when it can be withdrawn is untidy.
            let _ = probe.relay.revoke(&published.id);
            return fail(CheckName::Relay, error.message().to_string(), remedy());
        }
    };
    match crypto::open(&returned, &key) {
        Ok(plaintext) if plaintext == PAYLOAD => pass(
            CheckName::Relay,
            format!("{url} accepted and returned a sealed object"),
        ),
        Ok(_) => fail(
            CheckName::Relay,
            format!("{url} returned an object that decrypted to something else"),
            remedy(),
        ),
        Err(error) => fail(CheckName::Relay, error.message().to_string(), remedy()),
    }
}

/// The six probe checks plus the three that need a working inbox.
fn remote_checks(environment: &Environment<'_>, target: &TransportTarget) -> Vec<DoctorCheck> {
    let host = target.ssh_host().to_string();
    let mut checks = Vec::with_capacity(8);

    let probe = match environment.remote.probe(target) {
        Ok(report) => report,
        Err(error) => {
            // The probe itself could not run, which is a different thing from a
            // check inside it failing. Say so once, for each check it covers.
            for name in [
                CheckName::SshClient,
                CheckName::SftpClient,
                CheckName::HostResolution,
                CheckName::Authentication,
                CheckName::SftpSubsystem,
            ] {
                checks.push(fail(name, error.message().to_string(), ssh_remedy(&host)));
            }
            checks.push(fail(
                CheckName::RemoteHome,
                "not checked: the host could not be probed".to_string(),
                ssh_remedy(&host),
            ));
            checks.push(fail(
                CheckName::InboxPermissions,
                "not checked: the host could not be probed".to_string(),
                ssh_remedy(&host),
            ));
            checks.push(fail(
                CheckName::UploadAndCleanup,
                "not checked: the host could not be probed".to_string(),
                ssh_remedy(&host),
            ));
            return checks;
        }
    };

    checks.push(from_probe(
        &probe,
        CheckName::SshClient,
        "ssh client",
        &host,
    ));
    checks.push(from_probe(
        &probe,
        CheckName::SftpClient,
        "sftp client",
        &host,
    ));
    checks.push(host_resolution(environment, &host));
    checks.push(authentication(&probe, &host));
    checks.push(from_probe(
        &probe,
        CheckName::SftpSubsystem,
        "sftp subsystem",
        &host,
    ));

    // Everything below needs a usable inbox, so one failure legitimately makes
    // the rest unanswerable. They are still reported, as "not checked".
    match ensure_inbox(
        environment.remote,
        target,
        environment.remote_dir.as_deref(),
    ) {
        Err(error) => {
            let remedy = error.remedy().cloned().unwrap_or_else(|| ssh_remedy(&host));
            checks.push(fail(
                CheckName::RemoteHome,
                error.message().to_string(),
                remedy.clone(),
            ));
            checks.push(fail(
                CheckName::InboxPermissions,
                "not checked: the inbox could not be prepared".to_string(),
                remedy.clone(),
            ));
            checks.push(fail(
                CheckName::UploadAndCleanup,
                "not checked: the inbox could not be prepared".to_string(),
                remedy,
            ));
        }
        Ok(location) => {
            checks.push(pass(
                CheckName::RemoteHome,
                location.home().as_str().to_string(),
            ));
            let mut detail = format!("{} is private (0700)", location.root());
            if let Some(warning) = location.warning() {
                detail.push_str("; ");
                detail.push_str(&warning);
            }
            checks.push(pass(CheckName::InboxPermissions, detail));

            match verify_round_trip(
                environment.remote,
                environment.upload,
                target,
                location.root(),
            ) {
                Ok(()) => checks.push(pass(
                    CheckName::UploadAndCleanup,
                    "a test file was uploaded, verified and removed".to_string(),
                )),
                Err(error) => checks.push(fail(
                    CheckName::UploadAndCleanup,
                    error.message().to_string(),
                    error.remedy().cloned().unwrap_or_else(|| ssh_remedy(&host)),
                )),
            }
        }
    }

    checks
}

/// `ssh -G` resolving the alias, which is a different failure from the host
/// being unreachable: a typo in an alias looks like a dead machine otherwise.
fn host_resolution(environment: &Environment<'_>, host: &str) -> DoctorCheck {
    match environment.ssh_config.settings_for(host) {
        Ok(settings) => pass(CheckName::HostResolution, settings.summary()),
        Err(error) => fail(
            CheckName::HostResolution,
            error.message().to_string(),
            Remedy::new(
                "Check what your SSH configuration says about it:",
                format!("ssh -G {host}"),
            ),
        ),
    }
}

/// Authentication and `known_hosts` are one line in the specification because they fail
/// together from the user's point of view: either this host trusts you and you
/// trust it, or it does not.
fn authentication(probe: &crate::ports::ProbeReport, host: &str) -> DoctorCheck {
    for name in ["connection", "host key", "authentication"] {
        if let Some(entry) = probe
            .checks
            .iter()
            .find(|entry| entry.name == name && entry.status == CheckStatus::Fail)
        {
            return fail(
                CheckName::Authentication,
                format!("{name}: {}", entry.detail),
                ssh_remedy(host),
            );
        }
    }
    pass(
        CheckName::Authentication,
        "the host key matches known_hosts and authentication succeeded".to_string(),
    )
}

fn from_probe(
    probe: &crate::ports::ProbeReport,
    name: CheckName,
    probe_name: &str,
    host: &str,
) -> DoctorCheck {
    match probe.checks.iter().find(|entry| entry.name == probe_name) {
        Some(entry) if entry.status == CheckStatus::Fail => {
            fail(name, entry.detail.clone(), ssh_remedy(host))
        }
        Some(entry) => DoctorCheck {
            name,
            status: entry.status,
            detail: entry.detail.clone(),
            remedy: None,
        },
        None => DoctorCheck {
            name,
            status: CheckStatus::Warn,
            detail: "not reported by the probe".to_string(),
            remedy: None,
        },
    }
}

fn clipboard_check(environment: &Environment<'_>) -> DoctorCheck {
    let Some(clipboard) = environment.clipboard else {
        return not_built_in(CheckName::Clipboard, "reading the clipboard");
    };
    match clipboard.read_snapshot() {
        Ok(snapshot) if snapshot.is_empty() => pass(
            CheckName::Clipboard,
            "readable; currently empty".to_string(),
        ),
        Ok(_) => pass(CheckName::Clipboard, "readable".to_string()),
        Err(error) => fail(
            CheckName::Clipboard,
            error.message().to_string(),
            error
                .remedy()
                .cloned()
                .unwrap_or_else(|| Remedy::new("Copy something and try again:", "clift doctor")),
        ),
    }
}

fn config_check(environment: &Environment<'_>) -> DoctorCheck {
    let state = &environment.config;
    if let Some(error) = &state.error {
        return fail(
            CheckName::ConfigVersion,
            error.clone(),
            Remedy::new("Look at what is wrong with it:", "clift config validate"),
        );
    }
    if !state.exists {
        return DoctorCheck {
            name: CheckName::ConfigVersion,
            status: CheckStatus::Warn,
            detail: "no configuration file yet".to_string(),
            remedy: Some(Remedy::new(
                "Create one by setting up a host:",
                "clift setup <ssh-host>",
            )),
        };
    }
    if !state.warnings.is_empty() {
        return DoctorCheck {
            name: CheckName::ConfigVersion,
            status: CheckStatus::Warn,
            detail: state.warnings.join("; "),
            remedy: Some(Remedy::new("Check the file:", "clift config validate")),
        };
    }
    pass(
        CheckName::ConfigVersion,
        format!(
            "version {} of {}, no migration pending",
            state.version, state.supported
        ),
    )
}

/// A capability this build does not have. Never a pass: nothing was examined.
/// One of the eight host checks, on a machine with no host to check.
///
/// Still a warning rather than a skip, for the reason every other unexamined
/// check is: a hole the reader cannot see is worse than one they can. What
/// changes with a relay configured is what the warning *says*. On a machine
/// using Universal Mode these eight are the only yellow lines in the report,
/// and telling that user to "configure a host first" sends them to set up a
/// mode they deliberately are not using. Eight lines of advice nobody should
/// follow is how a report teaches people to stop reading it.
fn no_fast_mode_host(name: CheckName, relay_configured: bool) -> DoctorCheck {
    let (detail, lead) = if relay_configured {
        (
            "not checked: no Fast Mode host is configured, and Universal Mode does not need one",
            "Set one up only if you want Fast Mode as well:",
        )
    } else {
        (
            "not checked: no target is configured",
            "Configure a host first:",
        )
    };
    DoctorCheck {
        name,
        status: CheckStatus::Warn,
        detail: detail.to_string(),
        remedy: Some(Remedy::new(lead, "clift setup <ssh-host>")),
    }
}

fn not_built_in(name: CheckName, what: &str) -> DoctorCheck {
    DoctorCheck {
        name,
        status: CheckStatus::Warn,
        detail: format!("not checked: {what} is not built into this binary yet"),
        remedy: None,
    }
}

fn ssh_remedy(host: &str) -> Remedy {
    Remedy::new("Check the connection by hand:", format!("ssh {host}"))
}

fn pass(name: CheckName, detail: String) -> DoctorCheck {
    DoctorCheck {
        name,
        status: CheckStatus::Pass,
        detail,
        remedy: None,
    }
}

fn fail(name: CheckName, detail: String, remedy: Remedy) -> DoctorCheck {
    DoctorCheck {
        name,
        status: CheckStatus::Fail,
        detail,
        remedy: Some(remedy),
    }
}
