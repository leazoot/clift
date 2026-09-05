//! Universal Mode: the attachment travels as ciphertext, and the terminal
//! carries only a token.
//!
//! Fast Mode answers "which host?" before it sends anything, and everything
//! about it follows from that: a target, an SSH connection, an
//! SFTP upload. It is the better mode when the answer is known, and it stays
//! the default wherever it is.
//!
//! Universal Mode answers a different question, by not asking it. The
//! attachment is sealed locally, the ciphertext goes to a relay that cannot
//! read it, and what the user pastes into their terminal is a token. Whichever
//! host that terminal is talking to is the host whose `clift fetch` redeems
//! the token -- so the target is decided by where the keystrokes went, which is
//! a fact about the user's screen rather than an inference Clift made.
//!
//! That is the whole of the safety argument for dropping target resolution, and
//! it is worth stating plainly because the specification makes "an attachment reached
//! the wrong host" a zero-incident line. Clift still never guesses. It simply
//! stops being the party that decides.
//!
//! What lives here is only the part with no IO in it: the token, the envelope,
//! the frame inside the envelope, and the media policy. Publishing and fetching
//! are use cases; talking to a relay is an adapter.

pub mod bundle;
pub mod crypto;
pub mod media;
pub mod token;

pub use bundle::BundleEntry;
pub use token::{ObjectId, SealKey, Token};

use crate::error::{CliftError, ErrorKind, Remedy, Stage};
use std::time::Duration;

/// Ceiling on a sealed object, unless the user's configuration lowers it.
///
/// Eight mebibytes is a deliberate choice rather than a round number: a
/// full-screen Retina screenshot as PNG is one to three, and a relay is a
/// shared resource holding everything in memory. A user who needs to move
/// something larger has Fast Mode, which has no such limit because it does not
/// pass through anybody else's machine.
pub const DEFAULT_MAX_OBJECT_BYTES: u64 = 8 * 1024 * 1024;

/// How long an unclaimed object survives, unless configured otherwise.
///
/// Short on purpose. The window that matters is "user presses the key, then
/// pastes into the agent", which is seconds. Five minutes is generous for that
/// and still means a token left in a scrollback is worthless by the time
/// anybody scrolls back to it.
pub const DEFAULT_TTL: Duration = Duration::from_secs(5 * 60);

/// Longest TTL a relay will honour, and the longest one Clift will ask for.
///
/// A cap rather than a preference: an object that lives for hours is a
/// plaintext-equivalent secret sitting in somebody's shell history for hours.
pub const MAX_TTL: Duration = Duration::from_secs(60 * 60);

/// Where the relay is and what it is allowed to hold.
///
/// Constructed in the CLI from configuration and the environment, then handed
/// down. `clift-core` reads neither, which is what keeps the resolution order
/// in one visible place instead of spread across whoever needed a value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelaySettings {
    url: String,
    max_object_bytes: u64,
    ttl: Duration,
}

impl RelaySettings {
    /// # Errors
    /// Fails when the URL is empty, is not `http://` or `https://`, or when the
    /// TTL is zero or above [`MAX_TTL`].
    pub fn new(
        url: impl Into<String>,
        max_object_bytes: u64,
        ttl: Duration,
    ) -> Result<Self, CliftError> {
        let url = url.into().trim().trim_end_matches('/').to_string();
        if url.is_empty() {
            return Err(unconfigured());
        }
        if !(url.starts_with("http://") || url.starts_with("https://")) {
            return Err(config_error(format!(
                "the relay URL {url:?} must start with http:// or https://"
            )));
        }
        if url.contains(char::is_whitespace) || url.contains(char::is_control) {
            return Err(config_error("the relay URL contains whitespace"));
        }
        if max_object_bytes == 0 {
            return Err(config_error(
                "relay.max_bytes is zero, so nothing could be sent",
            ));
        }
        if ttl.is_zero() {
            return Err(config_error(
                "relay.ttl is zero, so every object would expire at once",
            ));
        }
        if ttl > MAX_TTL {
            return Err(config_error(format!(
                "relay.ttl is longer than the {} minute maximum",
                MAX_TTL.as_secs() / 60
            )));
        }
        Ok(Self {
            url,
            max_object_bytes,
            ttl,
        })
    }

    /// The base URL, with no trailing slash.
    #[must_use]
    pub fn url(&self) -> &str {
        &self.url
    }

    #[must_use]
    pub const fn max_object_bytes(&self) -> u64 {
        self.max_object_bytes
    }

    #[must_use]
    pub const fn ttl(&self) -> Duration {
        self.ttl
    }
}

/// The error a command produces when Universal Mode was asked for and no relay
/// is configured.
///
/// Its own function because the remedy is the useful part, and it must read the
/// same wherever the discovery happens.
#[must_use]
pub fn unconfigured() -> CliftError {
    CliftError::new(
        Stage::Relay,
        ErrorKind::Config,
        "Universal Mode needs a relay, and none is configured",
    )
    .with_remedy(Remedy::new(
        "Point Clift at one, or run your own (see README):",
        "clift config set relay.url https://relay.example.com",
    ))
}

/// The same discovery made by `clift copy`, on the machine sending back.
///
/// A third wording rather than a reuse of either other one, because both would
/// be wrong here in a way the reader would notice. [`unconfigured`] tells them
/// to pick a relay, but the only relay that helps is the one the machine they
/// paste on already uses. [`unconfigured_on_receiver`] talks about redeeming a
/// token, and nothing is being redeemed: this run is about to create one.
#[must_use]
pub fn unconfigured_for_return() -> CliftError {
    CliftError::new(
        Stage::Relay,
        ErrorKind::Config,
        "no relay is configured on this host, so there is nowhere to leave the attachment",
    )
    .with_remedy(Remedy::new(
        "Use the same relay as the machine you paste on; `clift status` there shows its address:",
        "clift config set relay.url <relay-url-from-the-other-machine>",
    ))
}

/// The same discovery made on the receiving host, by `clift fetch`.
///
/// The sender's wording would be wrong here: the person reading this is not
/// choosing a relay, they are on a host that has never heard of the one the
/// sender already uses. A token deliberately carries no relay address, so the
/// only place the address exists is the sending machine, and the remedy says
/// where to look for it. The command is usually run by an agent, which will
/// show this to the user as it is.
#[must_use]
pub fn unconfigured_on_receiver() -> CliftError {
    CliftError::new(
        Stage::Relay,
        ErrorKind::Config,
        "no relay is configured on this host, so the token cannot be redeemed here",
    )
    .with_remedy(Remedy::new(
        "The machine that sent it already uses one; `clift status` there shows its address. \
         Point this host at the same relay, once:",
        "clift config set relay.url <relay-url-from-the-sender>",
    ))
}

fn config_error(message: impl Into<String>) -> CliftError {
    CliftError::new(Stage::Relay, ErrorKind::Config, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_trailing_slash_is_removed_so_paths_join_predictably() {
        let settings = RelaySettings::new("https://relay.example.com/", 1024, DEFAULT_TTL)
            .unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(settings.url(), "https://relay.example.com");
    }

    #[test]
    fn a_url_that_is_not_http_is_refused() {
        for url in ["", "  ", "relay.example.com", "ftp://relay", "file:///etc"] {
            assert!(RelaySettings::new(url, 1024, DEFAULT_TTL).is_err(), "{url}");
        }
    }

    #[test]
    fn a_ttl_beyond_the_cap_is_refused_rather_than_clamped() {
        let error = RelaySettings::new(
            "https://relay.example.com",
            1024,
            MAX_TTL + Duration::from_secs(1),
        )
        .unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Config);
    }

    #[test]
    fn zero_values_are_refused() {
        assert!(RelaySettings::new("https://r", 0, DEFAULT_TTL).is_err());
        assert!(RelaySettings::new("https://r", 1, Duration::ZERO).is_err());
    }

    #[test]
    fn an_unconfigured_relay_says_how_to_configure_one() {
        let error = unconfigured();
        assert_eq!(error.kind(), ErrorKind::Config);
        assert!(
            error
                .remedy()
                .is_some_and(|remedy| remedy.command().contains("relay.url"))
        );
    }
}
