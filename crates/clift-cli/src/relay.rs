//! Where the relay settings come from, resolved in one place.
//!
//! Three sources, and the order is the usual one for a command line tool: the
//! environment beats the configuration file, because the environment is the
//! more immediate instruction. `clift-core` reads neither -- it is handed
//! finished [`RelaySettings`] -- so this is the only file where the precedence
//! is decided, and therefore the only one to read when it surprises somebody.
//!
//! | Variable | Overrides |
//! | --- | --- |
//! | `CLIFT_RELAY_URL` | `relay.url` |
//! | `CLIFT_RELAY_MAX_BYTES` | `relay.max_bytes` |
//! | `CLIFT_RELAY_TTL` | `relay.ttl` |
//!
//! The same three names the relay itself reads, so a developer running both
//! ends locally sets them once.
//!
//! Reading the environment and deciding what to do with it are separate
//! functions on purpose. The decision is the part worth testing, and setting a
//! process-wide variable to test it would need `unsafe` in a crate that forbids
//! it -- and would make the tests order-dependent besides.

use clift_core::config::Config;
use clift_core::config::units::{parse_duration, parse_size};
use clift_core::error::{CliftError, ErrorKind, Stage};
use clift_core::universal::{
    DEFAULT_MAX_OBJECT_BYTES, DEFAULT_TTL, RelaySettings, unconfigured, unconfigured_on_receiver,
};

/// What the environment says, if it says anything.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Overrides {
    url: Option<String>,
    max_bytes: Option<String>,
    ttl: Option<String>,
}

impl Overrides {
    /// Reads the three variables. An empty value counts as unset, because an
    /// exported-but-empty variable is how a shell script says "no".
    #[must_use]
    pub fn from_process() -> Self {
        let read = |name: &str| {
            std::env::var(name)
                .ok()
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty())
        };
        Self {
            url: read("CLIFT_RELAY_URL"),
            max_bytes: read("CLIFT_RELAY_MAX_BYTES"),
            ttl: read("CLIFT_RELAY_TTL"),
        }
    }
}

/// The relay this run should use, or the reason there is none.
///
/// # Errors
/// Returns the "no relay configured" error when neither the environment nor the
/// configuration names one, and a configuration error when what they name
/// cannot be used.
pub fn settings(config: &Config) -> Result<RelaySettings, CliftError> {
    resolve(config, &Overrides::from_process())
}

/// The relay `clift fetch` redeems the token at.
///
/// The same resolution as [`settings`]; only "there is none" reads differently,
/// because on the receiving host the fix is not "pick a relay" but "use the
/// sender's", and the person reading it is often an agent relaying the text.
///
/// # Errors
/// As [`settings`], with [`unconfigured_on_receiver`] in place of
/// [`unconfigured`].
pub fn settings_for_fetch(config: &Config) -> Result<RelaySettings, CliftError> {
    if !is_configured(config) {
        return Err(unconfigured_on_receiver());
    }
    settings(config)
}

/// Whether a relay is configured at all, without validating one.
///
/// Used where the answer only affects what to say: `status` lists it, `doctor`
/// checks it. Somewhere that is about to use the relay calls [`settings`] and
/// gets the validation with it.
#[must_use]
pub fn is_configured(config: &Config) -> bool {
    config.relay().is_some() || Overrides::from_process().url.is_some()
}

fn resolve(config: &Config, overrides: &Overrides) -> Result<RelaySettings, CliftError> {
    let configured = config.relay();

    let url = match (&overrides.url, configured) {
        (Some(url), _) => url.clone(),
        (None, Some(relay)) => relay.url().to_string(),
        (None, None) => return Err(unconfigured()),
    };

    let max_bytes = match &overrides.max_bytes {
        Some(value) => parse_size(value)
            .map_err(|error| environment_error("CLIFT_RELAY_MAX_BYTES", error.reason()))?,
        None => configured.map_or(
            DEFAULT_MAX_OBJECT_BYTES,
            clift_core::config::RelayConfig::max_bytes,
        ),
    };

    let ttl = match &overrides.ttl {
        Some(value) => parse_duration(value)
            .map_err(|error| environment_error("CLIFT_RELAY_TTL", error.reason()))?,
        None => configured.map_or(DEFAULT_TTL, clift_core::config::RelayConfig::ttl),
    };

    RelaySettings::new(url, max_bytes, ttl)
}

fn environment_error(variable: &str, reason: &str) -> CliftError {
    CliftError::new(
        Stage::Config,
        ErrorKind::Config,
        format!("{variable} cannot be used: {reason}"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn overrides(url: Option<&str>, max_bytes: Option<&str>, ttl: Option<&str>) -> Overrides {
        Overrides {
            url: url.map(str::to_string),
            max_bytes: max_bytes.map(str::to_string),
            ttl: ttl.map(str::to_string),
        }
    }

    fn config_with_relay(url: &str) -> Config {
        clift_core::config::parse(&format!(
            "version = 1\n[relay]\nurl = \"{url}\"\nmax_bytes = \"2MiB\"\nttl = \"90s\"\n"
        ))
        .unwrap_or_else(|error| panic!("{error}"))
        .config
    }

    fn empty_config() -> Config {
        clift_core::config::parse("version = 1\n")
            .unwrap_or_else(|error| panic!("{error}"))
            .config
    }

    #[test]
    fn the_file_is_used_when_the_environment_says_nothing() {
        let settings = resolve(
            &config_with_relay("https://relay.example.com"),
            &Overrides::default(),
        )
        .unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(settings.url(), "https://relay.example.com");
        assert_eq!(settings.max_object_bytes(), 2 * 1024 * 1024);
        assert_eq!(settings.ttl(), Duration::from_secs(90));
    }

    #[test]
    fn the_environment_wins_field_by_field() {
        let settings = resolve(
            &config_with_relay("https://relay.example.com"),
            &overrides(Some("http://127.0.0.1:8787"), None, Some("30s")),
        )
        .unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(settings.url(), "http://127.0.0.1:8787");
        assert_eq!(settings.ttl(), Duration::from_secs(30));
        // Untouched by the environment, so still the file's value.
        assert_eq!(settings.max_object_bytes(), 2 * 1024 * 1024);
    }

    #[test]
    fn no_relay_anywhere_is_reported_as_such() {
        let error = resolve(&empty_config(), &Overrides::default()).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Config);
        assert!(
            error
                .remedy()
                .is_some_and(|remedy| remedy.command().contains("relay.url"))
        );
    }

    /// On the receiving host the missing relay is the sender's, and the text
    /// has to say so: an agent will show it to a user who is looking at a
    /// different machine from the one that needs the setting.
    #[test]
    fn fetch_without_a_relay_points_at_the_sender_for_the_address() {
        let error = settings_for_fetch(&empty_config()).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Config);
        assert!(error.message().contains("this host"), "{}", error.message());
        let remedy = error.remedy().expect("a remedy");
        assert!(
            remedy.description().contains("clift status"),
            "{}",
            remedy.description()
        );
        assert!(
            remedy.command().starts_with("clift config set relay.url"),
            "{}",
            remedy.command()
        );

        // With a relay it is the ordinary resolution, environment included.
        let settings = settings_for_fetch(&config_with_relay("https://relay.example.com"))
            .unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(settings.url(), "https://relay.example.com");
    }

    #[test]
    fn an_environment_variable_that_cannot_be_parsed_names_itself() {
        let error = resolve(
            &config_with_relay("https://relay.example.com"),
            &overrides(None, Some("quite big"), None),
        )
        .unwrap_err();
        assert!(
            error.message().contains("CLIFT_RELAY_MAX_BYTES"),
            "{}",
            error.message()
        );

        let error = resolve(
            &config_with_relay("https://relay.example.com"),
            &overrides(None, None, Some("soon")),
        )
        .unwrap_err();
        assert!(
            error.message().contains("CLIFT_RELAY_TTL"),
            "{}",
            error.message()
        );
    }

    #[test]
    fn an_environment_url_alone_is_enough_and_brings_the_defaults_with_it() {
        let settings = resolve(
            &empty_config(),
            &overrides(Some("http://127.0.0.1:8787"), None, None),
        )
        .unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(settings.max_object_bytes(), DEFAULT_MAX_OBJECT_BYTES);
        assert_eq!(settings.ttl(), DEFAULT_TTL);
    }

    /// A URL from the environment is validated exactly as one from the file is.
    /// It is the more likely of the two to be wrong.
    #[test]
    fn a_url_the_environment_supplies_is_still_checked() {
        for bad in ["relay.example.com", "ftp://relay", "file:///etc/passwd"] {
            assert!(
                resolve(&empty_config(), &overrides(Some(bad), None, None)).is_err(),
                "accepted {bad}"
            );
        }
    }
}
