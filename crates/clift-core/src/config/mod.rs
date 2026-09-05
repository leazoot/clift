//! `config.toml`: the only state Clift persists.
//!
//! Parsing is strict in the way the specification asks for: a misspelled key produces a
//! warning rather than being silently dropped, a wrong type fails with the
//! offending field named, and a file written by a newer Clift is refused
//! instead of being reinterpreted with today's meaning.

pub mod edit;
pub mod io;
pub mod migrate;
pub mod schema;
pub mod units;

use crate::domain::{DomainError, Limits, RemotePath, TargetName};
use crate::error::{CliftError, ErrorKind, Stage};
use crate::hotkey::Hotkey;
use crate::universal::{DEFAULT_MAX_OBJECT_BYTES, DEFAULT_TTL};
use schema::{RawConfig, RawTarget, SUPPORTED_VERSION};
use std::collections::BTreeMap;
use std::time::Duration;

/// Which of the two ways of getting an attachment across a command uses.
///
/// The two are not variants of one mechanism; they are different mechanisms
/// with different trust models, and the type exists so that no code path can
/// end up "somewhere in between". A command either resolved a target and is
/// about to open an SSH connection, or it did not and is about to seal
/// something for a relay.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Mode {
    /// SFTP over the user's own SSH, to a target chosen before anything is
    /// sent. The original mode, and still the default for a configuration that
    /// has no relay -- which is every configuration written before v2.0.
    #[default]
    Fast,
    /// Sealed object, relay, token. No target.
    Universal,
}

impl Mode {
    fn parse(value: &str) -> Result<Self, DomainError> {
        match value {
            "fast" => Ok(Mode::Fast),
            "universal" => Ok(Mode::Universal),
            other => Err(DomainError::new(
                "mode",
                format!("unknown mode {other:?}; use \"fast\" or \"universal\""),
            )),
        }
    }

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Mode::Fast => "fast",
            Mode::Universal => "universal",
        }
    }
}

/// The relay half of the configuration.
///
/// Present in the file or not at all: an empty `[relay]` section with no `url`
/// is the same as no section, because there is nothing useful a relay setting
/// can mean without somewhere to send it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelayConfig {
    url: String,
    max_bytes: u64,
    ttl: Duration,
}

impl RelayConfig {
    #[must_use]
    pub fn url(&self) -> &str {
        &self.url
    }

    #[must_use]
    pub const fn max_bytes(&self) -> u64 {
        self.max_bytes
    }

    #[must_use]
    pub const fn ttl(&self) -> Duration {
        self.ttl
    }
}

/// How long an idle multiplexed SSH connection is kept before it closes.
///
/// Ten minutes is the specification's default. Long enough that a session of pasting
/// reuses one handshake throughout, short enough that a laptop closed for lunch
/// is not still holding a socket open when it wakes.
pub const DEFAULT_CONNECTION_PERSIST: Duration = Duration::from_secs(10 * 60);

/// The longest Clift will keep one.
///
/// A cap rather than a preference, and the reason is the same one that keeps
/// Clift out of the background generally: a reused connection is a
/// real `ssh` process waiting on a socket. One that outlives the working day
/// is a daemon by another name, whatever it is called in the configuration.
pub const MAX_CONNECTION_PERSIST: Duration = Duration::from_secs(60 * 60);

/// Connection reuse: the specification's half of the configuration.
///
/// Absent from a file means the defaults, which is reuse on at ten minutes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Connection {
    reuse: bool,
    persist: Duration,
}

impl Default for Connection {
    fn default() -> Self {
        Self {
            reuse: true,
            persist: DEFAULT_CONNECTION_PERSIST,
        }
    }
}

impl Connection {
    /// # Errors
    /// Fails when `persist` is zero or longer than [`MAX_CONNECTION_PERSIST`].
    ///
    /// Zero is refused rather than passed on because OpenSSH reads a zero
    /// `ControlPersist` as *forever*: the one value a user would write to mean
    /// "do not leave anything running" is the one that leaves it running until
    /// the machine is restarted. `reuse = false` is how to say that, and the
    /// error says so.
    pub fn new(reuse: bool, persist: Duration) -> Result<Self, CliftError> {
        if persist.is_zero() {
            return Err(config_error(DomainError::new(
                "connection.persist",
                "a zero is how OpenSSH spells \"keep the connection forever\"; \
                 write reuse = false to turn reuse off instead",
            )));
        }
        if persist > MAX_CONNECTION_PERSIST {
            return Err(config_error(DomainError::new(
                "connection.persist",
                format!(
                    "longer than the {} minute maximum",
                    MAX_CONNECTION_PERSIST.as_secs() / 60
                ),
            )));
        }
        Ok(Self { reuse, persist })
    }

    /// Whether a connection may be shared between invocations.
    #[must_use]
    pub const fn reuse(&self) -> bool {
        self.reuse
    }

    /// How long an idle shared connection is kept.
    #[must_use]
    pub const fn persist(&self) -> Duration {
        self.persist
    }
}

/// The insertion text profile used for a target.
///
/// Only `instruction` exists in v0.1: the other built-in profiles of the specification are
/// backlog, and accepting names Clift cannot honour would be a lie.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Format {
    #[default]
    Instruction,
}

impl Format {
    fn parse(value: &str) -> Result<Self, DomainError> {
        match value {
            "instruction" => Ok(Format::Instruction),
            other => Err(DomainError::new(
                "format",
                format!("unknown profile {other:?}; the only supported value is \"instruction\""),
            )),
        }
    }

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Format::Instruction => "instruction",
        }
    }
}

/// Batch limits and formatting defaults shared by every target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Defaults {
    limits: Limits,
    retention: Duration,
    format: Format,
}

impl Defaults {
    #[must_use]
    pub const fn limits(&self) -> Limits {
        self.limits
    }

    #[must_use]
    pub const fn retention(&self) -> Duration {
        self.retention
    }

    #[must_use]
    pub const fn format(&self) -> Format {
        self.format
    }
}

impl Default for Defaults {
    fn default() -> Self {
        Self {
            limits: Limits::default(),
            retention: Duration::from_secs(24 * 60 * 60),
            format: Format::Instruction,
        }
    }
}

/// One configured destination.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    ssh_host: String,
    remote_dir: String,
    format: Option<Format>,
    remote_home: Option<RemotePath>,
    last_success_at: Option<String>,
}

/// Default inbox location, relative to the remote home directory.
pub const DEFAULT_REMOTE_DIR: &str = "~/.cache/clift/inbox";

impl Target {
    /// A target as `setup` first learns it: an alias and where the inbox goes.
    ///
    /// `remote_home` and `last_success_at` are filled in afterwards, because
    /// they are things the host told Clift rather than things the user chose.
    #[must_use]
    pub fn new(ssh_host: impl Into<String>, remote_dir: impl Into<String>) -> Self {
        Self {
            ssh_host: ssh_host.into(),
            remote_dir: remote_dir.into(),
            format: None,
            remote_home: None,
            last_success_at: None,
        }
    }

    /// Caches the resolved remote home, saving a round trip on every send.
    #[must_use]
    pub fn with_remote_home(mut self, home: RemotePath) -> Self {
        self.remote_home = Some(home);
        self
    }

    /// Records that the host worked at this instant, RFC 3339 in UTC.
    #[must_use]
    pub fn with_last_success_at(mut self, timestamp: impl Into<String>) -> Self {
        self.last_success_at = Some(timestamp.into());
        self
    }

    /// The SSH host alias, passed to the system `ssh` verbatim.
    #[must_use]
    pub fn ssh_host(&self) -> &str {
        &self.ssh_host
    }

    /// Inbox location as written by the user; still unresolved, so it may
    /// start with `~` or reference `$XDG_CACHE_HOME`.
    #[must_use]
    pub fn remote_dir(&self) -> &str {
        &self.remote_dir
    }

    #[must_use]
    pub const fn format(&self) -> Option<Format> {
        self.format
    }

    /// Absolute remote home cached by `setup`. Not sensitive, and saves a round
    /// trip on every send.
    #[must_use]
    pub const fn remote_home(&self) -> Option<&RemotePath> {
        self.remote_home.as_ref()
    }

    #[must_use]
    pub fn last_success_at(&self) -> Option<&str> {
        self.last_success_at.as_deref()
    }
}

/// A validated, immutable configuration.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Config {
    mode: Option<Mode>,
    default_target: Option<TargetName>,
    defaults: Defaults,
    targets: BTreeMap<TargetName, Target>,
    relay: Option<RelayConfig>,
    hotkey: Option<Hotkey>,
    connection: Option<Connection>,
}

impl Config {
    /// The schema version this configuration is expressed in.
    #[must_use]
    pub const fn version(&self) -> u32 {
        SUPPORTED_VERSION
    }

    /// Which mode a command uses when it was not told, considering only this
    /// file.
    ///
    /// Prefer [`Config::mode_with_relay`] anywhere the environment might also
    /// have a say. This exists for callers that genuinely mean "what does the
    /// file say", and it is written in terms of the other one so the two cannot
    /// drift apart.
    #[must_use]
    pub fn mode(&self) -> Mode {
        self.mode_with_relay(self.relay.is_some())
    }

    /// Which mode a command uses when it was not told.
    ///
    /// The resolution is deliberately boring, and it is a resolution rather
    /// than a stored default because the honest answer depends on whether the
    /// user has a relay at all:
    ///
    /// 1. `mode` in the file, if it is set. The user said so.
    /// 2. Universal, if a relay is configured. Configuring one is the act of
    ///    opting in; there is nothing else it could be for.
    /// 3. Fast. Which is what every configuration written before v2.0 gets,
    ///    unchanged, because none of them has a relay.
    ///
    /// `relay_configured` is a parameter rather than read from `self` because a
    /// relay can also come from the environment, and `clift-core` does not read
    /// the environment. Passing it in is what keeps `clift status` from
    /// reporting a different mode than `clift paste` would actually use --
    /// which is exactly the bug this parameter was added to fix.
    #[must_use]
    pub fn mode_with_relay(&self, relay_configured: bool) -> Mode {
        match (self.mode, relay_configured) {
            (Some(mode), _) => mode,
            (None, true) => Mode::Universal,
            (None, false) => Mode::Fast,
        }
    }

    /// The mode as written in the file, if it was written.
    #[must_use]
    pub const fn configured_mode(&self) -> Option<Mode> {
        self.mode
    }

    #[must_use]
    pub const fn relay(&self) -> Option<&RelayConfig> {
        self.relay.as_ref()
    }

    /// The combination as written in the file, if it was written.
    ///
    /// `None` rather than the default, so a caller can tell "the user chose
    /// this" from "nobody has chosen": only the first is worth writing back.
    #[must_use]
    pub const fn hotkey(&self) -> Option<&Hotkey> {
        self.hotkey.as_ref()
    }

    /// Connection reuse, resolved: the defaults when the file says nothing.
    #[must_use]
    pub fn connection(&self) -> Connection {
        self.connection.unwrap_or_default()
    }

    /// Connection reuse as written in the file, if it was written.
    ///
    /// `None` rather than the defaults, so that saving the file does not
    /// invent a section the user never asked for.
    #[must_use]
    pub const fn configured_connection(&self) -> Option<&Connection> {
        self.connection.as_ref()
    }

    #[must_use]
    pub const fn default_target(&self) -> Option<&TargetName> {
        self.default_target.as_ref()
    }

    #[must_use]
    pub const fn defaults(&self) -> &Defaults {
        &self.defaults
    }

    #[must_use]
    pub const fn targets(&self) -> &BTreeMap<TargetName, Target> {
        &self.targets
    }

    #[must_use]
    pub fn target(&self, name: &TargetName) -> Option<&Target> {
        self.targets.get(name)
    }

    /// The same configuration with one target added or replaced.
    ///
    /// Returns a new value rather than mutating: `setup` must not leave a
    /// half-updated configuration behind if a later step fails, and the
    /// simplest way to guarantee that is for the old one to still be there.
    #[must_use]
    pub fn with_target(&self, name: TargetName, target: Target) -> Self {
        let mut next = self.clone();
        next.targets.insert(name, target);
        next
    }

    /// The same configuration with a different default target.
    #[must_use]
    pub fn with_default_target(&self, name: TargetName) -> Self {
        let mut next = self.clone();
        next.default_target = Some(name);
        next
    }

    /// The same configuration with one target gone.
    ///
    /// If the removed target was the default, the configuration is left with no
    /// default at all. Promoting another one would be Clift choosing where the
    /// user's next attachment goes, which the specification forbids.
    #[must_use]
    pub fn without_target(&self, name: &TargetName) -> Self {
        let mut next = self.clone();
        next.targets.remove(name);
        if next.default_target.as_ref() == Some(name) {
            next.default_target = None;
        }
        next
    }

    /// The same configuration with one target under a new name.
    ///
    /// The default follows the rename: the user renamed a target, they did not
    /// ask to stop using it.
    #[must_use]
    pub fn with_renamed_target(&self, from: &TargetName, to: TargetName, target: Target) -> Self {
        let mut next = self.clone();
        next.targets.remove(from);
        if next.default_target.as_ref() == Some(from) {
            next.default_target = Some(to.clone());
        }
        next.targets.insert(to, target);
        next
    }
}

/// A configuration together with anything questionable found while reading it.
///
/// Warnings are returned rather than printed: `clift-core` does no IO, and the
/// CLI is the only layer that knows they belong on stderr.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigLoad {
    pub config: Config,
    pub warnings: Vec<String>,
}

/// Parses a configuration document.
///
/// An empty string is a valid, empty configuration: a first run has no file yet
/// and must not be treated as an error.
///
/// # Errors
/// Returns exit code 20 for malformed TOML, a wrong field type, a value that
/// violates a domain invariant, or a `version` newer than this build supports.
pub fn parse(source: &str) -> Result<ConfigLoad, CliftError> {
    let mut document: toml::Table = source.parse().map_err(|error: toml::de::Error| {
        CliftError::new(
            Stage::Config,
            ErrorKind::Config,
            format!("config file is not valid TOML: {}", summarise(&error)),
        )
        .with_source(error)
    })?;

    // Migrations run before anything is interpreted: a newer schema may have
    // changed what today's fields mean, so reading them first would be a guess.
    let migration = migrate::migrate_to_current(&mut document)?;

    let mut warnings: Vec<String> = migration.notes;
    warnings.extend(
        schema::unknown_keys(&document)
            .into_iter()
            .map(|key| format!("unknown config key {key:?} was ignored")),
    );

    let raw: RawConfig = document
        .clone()
        .try_into()
        .map_err(|error: toml::de::Error| {
            CliftError::new(
                Stage::Config,
                ErrorKind::Config,
                format!("config file has an invalid value: {}", summarise(&error)),
            )
            .with_source(error)
        })?;

    let config = build(raw)?;
    Ok(ConfigLoad { config, warnings })
}

fn build(raw: RawConfig) -> Result<Config, CliftError> {
    let defaults = build_defaults(raw.defaults.unwrap_or_default())?;

    let mut targets = BTreeMap::new();
    for (name, raw_target) in raw.targets.unwrap_or_default() {
        let name = TargetName::new(name).map_err(config_error)?;
        let target = build_target(&name, raw_target)?;
        targets.insert(name, target);
    }

    let default_target = match raw.default_target {
        Some(name) => {
            let name = TargetName::new(name).map_err(config_error)?;
            if !targets.contains_key(&name) {
                return Err(CliftError::new(
                    Stage::Config,
                    ErrorKind::Config,
                    format!("default_target {name:?} is not a configured target"),
                )
                .with_remedy(crate::error::Remedy::new(
                    "List the configured targets:",
                    "clift target list",
                )));
            }
            Some(name)
        }
        None => None,
    };

    let mode = raw
        .mode
        .map(|value| Mode::parse(&value))
        .transpose()
        .map_err(config_error)?;

    let relay = build_relay(raw.relay.unwrap_or_default())?;

    // An empty `[hotkey]` section is a section that says nothing, exactly as
    // an empty `[relay]` is: the combination falls back to the default rather
    // than becoming a configuration error.
    let hotkey = raw
        .hotkey
        .unwrap_or_default()
        .combination
        .map(|value| Hotkey::parse(&value))
        .transpose()
        .map_err(config_error)?;

    let connection = build_connection(raw.connection.unwrap_or_default())?;

    Ok(Config {
        mode,
        default_target,
        defaults,
        targets,
        relay,
        hotkey,
        connection,
    })
}

/// Reads `[connection]`, or reports that it said nothing.
///
/// An empty section is not a half-configuration: it resolves to the defaults,
/// exactly as `[relay]` and `[hotkey]` do.
fn build_connection(raw: schema::RawConnection) -> Result<Option<Connection>, CliftError> {
    if raw.reuse.is_none() && raw.persist.is_none() {
        return Ok(None);
    }
    let default = Connection::default();
    let persist = match raw.persist {
        Some(value) => units::parse_duration(&value).map_err(config_error)?,
        None => default.persist(),
    };
    Ok(Some(Connection::new(
        raw.reuse.unwrap_or(default.reuse()),
        persist,
    )?))
}

/// Builds the relay settings, or none at all.
///
/// A `[relay]` section with sizes and a TTL but no `url` is not an error and
/// not a half-configured relay: it is a section that says nothing. Treating it
/// as a configuration error would punish a user for a leftover after they
/// removed the URL.
fn build_relay(raw: schema::RawRelay) -> Result<Option<RelayConfig>, CliftError> {
    let Some(url) = raw
        .url
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
    else {
        return Ok(None);
    };

    let max_bytes = match raw.max_bytes {
        Some(value) => units::parse_size(&value).map_err(config_error)?,
        None => DEFAULT_MAX_OBJECT_BYTES,
    };
    let ttl = match raw.ttl {
        Some(value) => units::parse_duration(&value).map_err(config_error)?,
        None => DEFAULT_TTL,
    };

    // Validated here rather than at use, so `clift config validate` catches a
    // bad relay URL instead of the user finding out mid-paste.
    crate::universal::RelaySettings::new(url.clone(), max_bytes, ttl)?;

    Ok(Some(RelayConfig {
        url,
        max_bytes,
        ttl,
    }))
}

fn build_defaults(raw: schema::RawDefaults) -> Result<Defaults, CliftError> {
    let fallback = Defaults::default();

    let max_file_size = match raw.max_file_size {
        Some(value) => units::parse_size(&value).map_err(config_error)?,
        None => fallback.limits().max_file_size(),
    };
    let max_batch_size = match raw.max_batch_size {
        Some(value) => units::parse_size(&value).map_err(config_error)?,
        None => fallback.limits().max_batch_size(),
    };
    let max_files = raw.max_files.unwrap_or(fallback.limits().max_files());
    let limits = Limits::new(max_file_size, max_batch_size, max_files).map_err(config_error)?;

    let retention = match raw.retention {
        Some(value) => units::parse_duration(&value).map_err(config_error)?,
        None => fallback.retention(),
    };
    let format = match raw.format {
        Some(value) => Format::parse(&value).map_err(config_error)?,
        None => fallback.format(),
    };

    Ok(Defaults {
        limits,
        retention,
        format,
    })
}

fn build_target(name: &TargetName, raw: RawTarget) -> Result<Target, CliftError> {
    let ssh_host = raw.ssh_host.trim().to_string();
    if ssh_host.is_empty() {
        return Err(config_error(DomainError::new(
            "ssh_host",
            format!("target {name} has an empty ssh_host"),
        )));
    }
    if ssh_host.chars().any(char::is_control) || ssh_host.chars().any(char::is_whitespace) {
        return Err(config_error(DomainError::new(
            "ssh_host",
            format!("target {name} has an ssh_host containing whitespace or control characters"),
        )));
    }

    let remote_dir = raw
        .remote_dir
        .unwrap_or_else(|| DEFAULT_REMOTE_DIR.to_string());
    if remote_dir.trim().is_empty() || remote_dir.chars().any(char::is_control) {
        return Err(config_error(DomainError::new(
            "remote_dir",
            format!("target {name} has an empty or malformed remote_dir"),
        )));
    }

    let format = raw
        .format
        .map(|value| Format::parse(&value))
        .transpose()
        .map_err(config_error)?;

    let remote_home = raw
        .remote_home
        .map(RemotePath::new)
        .transpose()
        .map_err(config_error)?;

    Ok(Target {
        ssh_host,
        remote_dir,
        format,
        remote_home,
        last_success_at: raw.last_success_at,
    })
}

/// Renders a TOML error on one line.
///
/// The full rendering carries the offending key and, for a syntax error, the
/// line and column. Both are required, but the summary must stay on a single
/// line so that a one-line error stays a one-line error.
fn summarise(error: &toml::de::Error) -> String {
    error
        .to_string()
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join("; ")
}

/// Every configuration rejection is a config-stage failure, exit code 20.
fn config_error(error: DomainError) -> CliftError {
    error.into_clift(Stage::Config, ErrorKind::Config)
}
