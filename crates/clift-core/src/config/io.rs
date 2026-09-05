//! Reading and writing `config.toml`.
//!
//! The write path is the interesting half. Clift's only persistent state is
//! this file, and `setup`, `target` and `config set` all rewrite it. A crash
//! part way through must never leave the user with a truncated config, so the
//! new contents are written to a fresh file in the same directory and moved
//! into place with a single rename.
//!
//! Local filesystem access sits here rather than behind a port: only *remote*
//! IO needs to be abstracted for testing, and a temporary directory is a
//! perfectly good test double for the local one.

use super::{Config, ConfigLoad};
use crate::error::{CliftError, ErrorKind, Remedy, Stage};
use crate::places::{self, Platform};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

/// Config files are private to the user; the directory too.
#[cfg(unix)]
const FILE_MODE: u32 = 0o600;
#[cfg(unix)]
const DIR_MODE: u32 = 0o700;

/// Resolves the configuration path for this platform (see `places`).
///
/// # Errors
/// Fails when the environment names no base directory, which would otherwise
/// leave Clift guessing where the user's files live.
pub fn default_config_path() -> Result<PathBuf, CliftError> {
    places::config_file(Platform::current(), &places::process_environment).map_err(|unlocated| {
        unlocated.into_error(Stage::Config, ErrorKind::Config, "the config file")
    })
}

/// Loads the configuration, treating a missing file as an empty configuration.
///
/// # Errors
/// Propagates parse failures, and reports a file that exists but cannot be read.
pub fn load(path: &Path) -> Result<ConfigLoad, CliftError> {
    match fs::read_to_string(path) {
        Ok(source) => super::parse(&source),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => super::parse(""),
        Err(error) => Err(CliftError::new(
            Stage::Config,
            ErrorKind::Config,
            format!("cannot read config file {}", path.display()),
        )
        .with_source(error)),
    }
}

/// Writes the configuration, replacing any previous file atomically.
///
/// # Errors
/// Fails if the directory cannot be created, the temporary file cannot be
/// written, or the rename fails. In every failure the previous file is left
/// exactly as it was.
pub fn save(path: &Path, config: &Config) -> Result<(), CliftError> {
    save_source(path, &render(config))
}

/// Reads the raw document, or an empty string when the file does not exist.
///
/// Used by `clift config set`, which edits the document in place so that keys
/// this build does not recognise survive the round trip.
///
/// # Errors
/// Fails when a file that exists cannot be read.
pub fn read_source(path: &Path) -> Result<String, CliftError> {
    match fs::read_to_string(path) {
        Ok(source) => Ok(source),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(error) => Err(CliftError::new(
            Stage::Config,
            ErrorKind::Config,
            format!("cannot read config file {}", path.display()),
        )
        .with_source(error)),
    }
}

/// Writes raw configuration text, replacing any previous file atomically.
///
/// # Errors
/// Fails if the directory cannot be created, the temporary file cannot be
/// written, or the rename fails. In every failure the previous file is left
/// exactly as it was.
pub fn save_source(path: &Path, rendered: &str) -> Result<(), CliftError> {
    let Some(directory) = path.parent() else {
        return Err(CliftError::new(
            Stage::Config,
            ErrorKind::Config,
            format!("config path {} has no parent directory", path.display()),
        ));
    };
    ensure_directory(directory)?;

    let temporary = TempFile::create_beside(path)?;
    temporary.write_all(rendered.as_bytes())?;
    temporary.persist(path)
}

#[cfg(unix)]
fn ensure_directory(directory: &Path) -> Result<(), CliftError> {
    // Permissions are set only on a directory Clift creates. Silently
    // tightening a directory the user already has would both surprise them and
    // hide a real permission problem behind a chmod.
    if directory.is_dir() {
        return Ok(());
    }
    fs::create_dir_all(directory).map_err(|error| directory_error(directory, error))?;
    fs::set_permissions(directory, fs::Permissions::from_mode(DIR_MODE))
        .map_err(|error| directory_error(directory, error))
}

#[cfg(not(unix))]
fn ensure_directory(directory: &Path) -> Result<(), CliftError> {
    // Windows uses ACLs rather than mode bits; tightening them is a v1.0 item.
    fs::create_dir_all(directory).map_err(|error| directory_error(directory, error))
}

fn directory_error(directory: &Path, error: std::io::Error) -> CliftError {
    CliftError::new(
        Stage::Config,
        ErrorKind::Config,
        format!("cannot prepare config directory {}", directory.display()),
    )
    .with_source(error)
}

/// A file that deletes itself unless it is explicitly persisted.
///
/// Without this, a failure between "created the temporary file" and "renamed it"
/// would litter the config directory with partial files that the next run might
/// pick up.
struct TempFile {
    path: PathBuf,
    file: Option<fs::File>,
}

impl TempFile {
    fn create_beside(target: &Path) -> Result<Self, CliftError> {
        let directory = target.parent().unwrap_or_else(|| Path::new("."));
        let stem = target
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("config.toml");

        // `create_new` fails rather than truncating, so the loop cannot clobber
        // a temporary file another process is currently writing.
        for attempt in 0..64u32 {
            let candidate = directory.join(format!(".{stem}.{attempt}.tmp"));
            let mut options = fs::OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            options.mode(FILE_MODE);

            match options.open(&candidate) {
                Ok(file) => {
                    // `mode` is masked by umask, so the permissions are set
                    // again explicitly: 0600 must hold whatever the user's
                    // umask happens to be.
                    #[cfg(unix)]
                    fs::set_permissions(&candidate, fs::Permissions::from_mode(FILE_MODE))
                        .map_err(|error| {
                            CliftError::new(
                                Stage::Config,
                                ErrorKind::Config,
                                format!("cannot set permissions on {}", candidate.display()),
                            )
                            .with_source(error)
                        })?;
                    return Ok(Self {
                        path: candidate,
                        file: Some(file),
                    });
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(error) => {
                    return Err(CliftError::new(
                        Stage::Config,
                        ErrorKind::Config,
                        format!("cannot create a temporary file in {}", directory.display()),
                    )
                    .with_source(error));
                }
            }
        }

        Err(CliftError::new(
            Stage::Config,
            ErrorKind::Config,
            format!(
                "cannot create a temporary file in {}: too many stale temporary files",
                directory.display()
            ),
        )
        .with_remedy(Remedy::new(
            "Remove the leftover files, then retry:",
            "rm -f \"$(dirname \"$(clift config path)\")\"/.config.toml.*.tmp",
        )))
    }

    fn write_all(&self, bytes: &[u8]) -> Result<(), CliftError> {
        let Some(mut file) = self.file.as_ref() else {
            return Err(CliftError::new(
                Stage::Config,
                ErrorKind::Internal,
                "temporary config file was already persisted",
            ));
        };
        let write = |file: &mut &fs::File| -> std::io::Result<()> {
            file.write_all(bytes)?;
            file.flush()?;
            // The rename is only atomic with respect to the contents if the
            // contents reached the disk first.
            file.sync_all()
        };
        write(&mut file).map_err(|error| {
            CliftError::new(
                Stage::Config,
                ErrorKind::Config,
                format!("cannot write config to {}", self.path.display()),
            )
            .with_source(error)
        })
    }

    fn persist(mut self, target: &Path) -> Result<(), CliftError> {
        self.file = None;
        fs::rename(&self.path, target).map_err(|error| {
            CliftError::new(
                Stage::Config,
                ErrorKind::Config,
                format!("cannot replace config file {}", target.display()),
            )
            .with_source(error)
        })?;
        // Ownership moves into this call, so the guard must not delete the file
        // it has just renamed away.
        std::mem::forget(self);
        Ok(())
    }
}

impl Drop for TempFile {
    fn drop(&mut self) {
        self.file = None;
        let _ = fs::remove_file(&self.path);
    }
}

/// Renders a configuration as TOML.
///
/// Written by hand rather than derived: `Config` must not carry a `Serialize`
/// implementation, or a later refactor of the domain types would silently
/// change the file format users have on disk.
fn render(config: &Config) -> String {
    let mut out = String::new();
    out.push_str(&format!("version = {}\n", config.version()));
    if let Some(mode) = config.configured_mode() {
        out.push_str(&format!("mode = {}\n", toml_string(mode.as_str())));
    }
    if let Some(default_target) = config.default_target() {
        out.push_str(&format!(
            "default_target = {}\n",
            toml_string(default_target.as_str())
        ));
    }

    let defaults = config.defaults();
    out.push_str("\n[defaults]\n");
    out.push_str(&format!(
        "max_file_size = {}\n",
        toml_string(&render_size(defaults.limits().max_file_size()))
    ));
    out.push_str(&format!(
        "max_batch_size = {}\n",
        toml_string(&render_size(defaults.limits().max_batch_size()))
    ));
    out.push_str(&format!("max_files = {}\n", defaults.limits().max_files()));
    out.push_str(&format!(
        "retention = {}\n",
        toml_string(&render_duration(defaults.retention()))
    ));
    out.push_str(&format!(
        "format = {}\n",
        toml_string(defaults.format().as_str())
    ));

    for (name, target) in config.targets() {
        out.push_str(&format!("\n[targets.{}]\n", toml_key(name.as_str())));
        out.push_str(&format!("ssh_host = {}\n", toml_string(target.ssh_host())));
        out.push_str(&format!(
            "remote_dir = {}\n",
            toml_string(target.remote_dir())
        ));
        if let Some(format) = target.format() {
            out.push_str(&format!("format = {}\n", toml_string(format.as_str())));
        }
        if let Some(home) = target.remote_home() {
            out.push_str(&format!("remote_home = {}\n", toml_string(home.as_str())));
        }
        if let Some(seen) = target.last_success_at() {
            out.push_str(&format!("last_success_at = {}\n", toml_string(seen)));
        }
    }

    // Every field of the relay is written out, including the two that have
    // defaults. Omitting the section is what a save must never do: a
    // configuration with no relay resolves to Fast Mode, so dropping it here
    // moves the user off Universal Mode and sends the next attachment to the
    // default target -- a host they did not name.
    if let Some(relay) = config.relay() {
        out.push_str("\n[relay]\n");
        out.push_str(&format!("url = {}\n", toml_string(relay.url())));
        out.push_str(&format!(
            "max_bytes = {}\n",
            toml_string(&render_size(relay.max_bytes()))
        ));
        out.push_str(&format!(
            "ttl = {}\n",
            toml_string(&render_duration(relay.ttl()))
        ));
    }

    if let Some(hotkey) = config.hotkey() {
        out.push_str("\n[hotkey]\n");
        out.push_str(&format!(
            "combination = {}\n",
            toml_string(&hotkey.render())
        ));
    }

    if let Some(connection) = config.configured_connection() {
        out.push_str("\n[connection]\n");
        out.push_str(&format!("reuse = {}\n", connection.reuse()));
        out.push_str(&format!(
            "persist = {}\n",
            toml_string(&render_duration(connection.persist()))
        ));
    }

    out
}

fn toml_string(value: &str) -> String {
    toml::Value::String(value.to_string()).to_string()
}

/// Bare keys are only legal for a restricted character set; anything else has
/// to be quoted, and target aliases may contain spaces or non-ASCII text.
fn toml_key(value: &str) -> String {
    let bare = !value.is_empty()
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
    if bare {
        value.to_string()
    } else {
        toml_string(value)
    }
}

fn render_size(bytes: u64) -> String {
    for (unit, scale) in [
        ("GiB", 1024 * 1024 * 1024),
        ("MiB", 1024 * 1024),
        ("KiB", 1024),
    ] {
        if bytes >= scale && bytes.is_multiple_of(scale) {
            return format!("{}{unit}", bytes / scale);
        }
    }
    format!("{bytes}B")
}

fn render_duration(duration: std::time::Duration) -> String {
    let seconds = duration.as_secs();
    for (unit, scale) in [("d", 86_400), ("h", 3_600), ("m", 60)] {
        if seconds >= scale && seconds.is_multiple_of(scale) {
            return format!("{}{unit}", seconds / scale);
        }
    }
    format!("{seconds}s")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_and_durations_round_trip_through_their_renderers() {
        for bytes in [0u64, 1, 1023, 1024, 50 * 1024 * 1024, 1024 * 1024 * 1024] {
            let rendered = render_size(bytes);
            assert_eq!(
                super::super::units::parse_size(&rendered).unwrap(),
                bytes,
                "size {bytes} rendered as {rendered:?} did not round trip"
            );
        }
        for seconds in [1u64, 59, 60, 3600, 86_400, 604_800] {
            let rendered = render_duration(std::time::Duration::from_secs(seconds));
            assert_eq!(
                super::super::units::parse_duration(&rendered)
                    .unwrap()
                    .as_secs(),
                seconds,
                "duration {seconds}s rendered as {rendered:?} did not round trip"
            );
        }
    }

    #[test]
    fn a_save_keeps_the_relay_and_the_mode() {
        // Regression. `render` once emitted neither, so every command that
        // saves the whole document -- `setup`, `target add`, `target use`,
        // `integrate` -- silently deleted `[relay]`. The user was moved from
        // Universal Mode back to Fast Mode without being told, and the next
        // attachment went to the default target.
        let source = concat!(
            "version = 1\n",
            "mode = \"universal\"\n",
            "default_target = \"core\"\n",
            "\n[targets.core]\n",
            "ssh_host = \"core\"\n",
            "remote_dir = \"~/.cache/clift/inbox\"\n",
            "\n[relay]\n",
            "url = \"http://127.0.0.1:9787\"\n",
            "max_bytes = \"8MiB\"\n",
            "ttl = \"5m\"\n",
        );
        let before = super::super::parse(source).unwrap().config;
        let rendered = render(&before);
        let after = super::super::parse(&rendered).unwrap().config;
        assert_eq!(before, after, "a save did not round trip:\n{rendered}");
    }

    /// Same class of bug, same guard. A hotkey the user chose is theirs, and
    /// a `setup` that dropped it would silently move them back to the default
    /// combination -- a key that stops working with no message anywhere.
    /// And once more for connection reuse. A `setup` that dropped
    /// `reuse = false` would put the user back on a shared connection they
    /// had deliberately turned off, which is the kind of change nobody
    /// notices until something else breaks.
    #[test]
    fn a_save_keeps_the_connection_settings() {
        let source = concat!(
            "version = 1\n",
            "\n[connection]\n",
            "reuse = false\n",
            "persist = \"30m\"\n",
        );
        let before = super::super::parse(source).unwrap().config;
        let connection = before.configured_connection().copied().unwrap();
        assert!(!connection.reuse());
        assert_eq!(connection.persist(), std::time::Duration::from_secs(1800));
        let rendered = render(&before);
        let after = super::super::parse(&rendered).unwrap().config;
        assert_eq!(before, after, "a save did not round trip:\n{rendered}");
    }

    #[test]
    fn a_save_keeps_a_chosen_hotkey() {
        let source = concat!(
            "version = 1\n",
            "\n[hotkey]\n",
            "combination = \"ctrl+alt+f9\"\n",
        );
        let before = super::super::parse(source).unwrap().config;
        assert_eq!(
            before.hotkey().map(crate::hotkey::Hotkey::render),
            Some("ctrl+alt+f9".to_string())
        );
        let rendered = render(&before);
        let after = super::super::parse(&rendered).unwrap().config;
        assert_eq!(before, after, "a save did not round trip:\n{rendered}");
    }

    #[test]
    fn a_save_does_not_move_a_relay_only_config_back_to_fast_mode() {
        // The shape the bug was found in: a configuration with a relay and no
        // explicit `mode`, which is what `clift config set relay.url` leaves
        // behind. Universal Mode here is a resolution, not a stored value, so
        // losing the relay loses the mode with it.
        let source = concat!(
            "version = 1\n",
            "default_target = \"core\"\n",
            "\n[targets.core]\n",
            "ssh_host = \"core\"\n",
            "remote_dir = \"~/.cache/clift/inbox\"\n",
            "\n[relay]\n",
            "url = \"http://127.0.0.1:9787\"\n",
        );
        let before = super::super::parse(source).unwrap().config;
        assert_eq!(before.mode(), super::super::Mode::Universal);

        let after = super::super::parse(&render(&before)).unwrap().config;
        assert_eq!(
            after.mode(),
            super::super::Mode::Universal,
            "saving the configuration moved the user back to Fast Mode"
        );
        assert_eq!(
            after.relay().map(super::super::RelayConfig::url),
            Some("http://127.0.0.1:9787")
        );
    }

    #[test]
    fn aliases_needing_quotes_are_quoted() {
        assert_eq!(toml_key("core"), "core");
        assert_eq!(toml_key("dev-vps_2"), "dev-vps_2");
        assert_eq!(toml_key("my host"), "\"my host\"");
        assert_eq!(toml_key("主机"), "\"主机\"");
    }
}
