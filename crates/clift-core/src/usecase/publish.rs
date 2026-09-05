//! Universal Mode, sending half: seal, publish, and hand back a token.
//!
//! The order is the requirement, and it is not the same order as `send`:
//!
//! 1. **check the limits** -- before a byte is encrypted, let alone uploaded;
//! 2. **draw a fresh key and nonce** -- from the OS CSPRNG, or abort;
//! 3. **seal** -- locally, so the relay only ever holds ciphertext;
//! 4. **publish** -- the ciphertext, and nothing else. No key, no file name, no
//!    media type: everything descriptive is inside the envelope;
//! 5. **build the token** -- only now, from an id the relay chose and a key the
//!    relay never saw.
//!
//! What is conspicuously absent is target resolution. That is the point of the
//! mode, and the reason it is safe to be absent: the token has to be typed
//! somewhere before it means anything, and where it is typed is the answer to
//! the question this use case did not ask.

use crate::error::{CliftError, ErrorKind, Remedy, Stage};
use crate::ports::{Randomness, Relay};
use crate::universal::crypto::{NONCE_BYTES, sealed_len};
use crate::universal::token::{SEAL_KEY_BYTES, SealKey};
use crate::universal::{BundleEntry, RelaySettings, Token, bundle, crypto};
use std::time::Duration;

/// A published object and everything the caller needs to talk about it.
#[derive(Debug)]
pub struct Published {
    token: Token,
    ttl: Duration,
    sealed_bytes: u64,
    entries: Vec<PublishedEntry>,
}

/// One attachment, as the sender described it.
///
/// Reported so the user can see what went, and included in `--json`. The remote
/// path is deliberately not here: at this moment nobody knows it, because the
/// host that will write the file has not been chosen yet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublishedEntry {
    name: String,
    media_type: String,
    size: u64,
}

impl PublishedEntry {
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    #[must_use]
    pub fn media_type(&self) -> &str {
        &self.media_type
    }

    #[must_use]
    pub const fn size(&self) -> u64 {
        self.size
    }
}

impl Published {
    #[must_use]
    pub const fn token(&self) -> &Token {
        &self.token
    }

    #[must_use]
    pub const fn ttl(&self) -> Duration {
        self.ttl
    }

    /// Size of the ciphertext, which is what the relay is holding.
    #[must_use]
    pub const fn sealed_bytes(&self) -> u64 {
        self.sealed_bytes
    }

    #[must_use]
    pub fn entries(&self) -> &[PublishedEntry] {
        &self.entries
    }
}

/// Seals the attachments and publishes them.
///
/// # Errors
/// Returns [`ErrorKind::LimitExceeded`] when the sealed object would exceed the
/// configured ceiling, [`ErrorKind::Internal`] when the random source is
/// unavailable, and whatever the relay reported otherwise.
pub fn publish(
    entries: &[BundleEntry],
    settings: &RelaySettings,
    relay: &dyn Relay,
    random: &dyn Randomness,
) -> Result<Published, CliftError> {
    let frame = bundle::encode(entries)?;

    // Checked against the sealed size rather than the plaintext size, because
    // the sealed size is what the relay will measure. Being refused by the
    // relay after a full upload is a worse way to learn this.
    let projected = u64::try_from(sealed_len(frame.len())).unwrap_or(u64::MAX);
    if projected > settings.max_object_bytes() {
        return Err(CliftError::new(
            Stage::Relay,
            ErrorKind::LimitExceeded,
            format!(
                "the attachment comes to {projected} bytes once sealed, over the {} the relay accepts",
                settings.max_object_bytes()
            ),
        )
        .with_remedy(Remedy::new(
            "Send it over your own SSH connection instead:",
            "clift send --clipboard --to <target>",
        )));
    }

    let mut key_bytes = [0_u8; SEAL_KEY_BYTES];
    let mut nonce = [0_u8; NONCE_BYTES];
    random.fill(&mut key_bytes)?;
    random.fill(&mut nonce)?;
    let key = SealKey::from_bytes(key_bytes);
    key_bytes.fill(0);

    let sealed = crypto::seal(&frame, &key, &nonce)?;
    let published = relay.publish(&sealed, settings.ttl())?;

    Ok(Published {
        token: Token::new(published.id, key),
        ttl: published.ttl,
        sealed_bytes: u64::try_from(sealed.len()).unwrap_or(u64::MAX),
        entries: entries
            .iter()
            .map(|entry| PublishedEntry {
                name: entry.name().as_str().to_string(),
                media_type: entry.media_type().to_string(),
                size: u64::try_from(entry.data().len()).unwrap_or(u64::MAX),
            })
            .collect(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::SafeFileName;
    use crate::testing::{CountingRandomness, FailingRandomness, RecordingRelay};
    use crate::universal::DEFAULT_TTL;

    fn settings(max: u64) -> RelaySettings {
        RelaySettings::new("https://relay.example.com", max, DEFAULT_TTL)
            .unwrap_or_else(|error| panic!("{error}"))
    }

    fn entry(name: &str, data: &[u8]) -> BundleEntry {
        BundleEntry::new(
            SafeFileName::new(name).unwrap_or_else(|error| panic!("{error}")),
            "image/png",
            data.to_vec(),
        )
        .unwrap_or_else(|error| panic!("{error}"))
    }

    #[test]
    fn the_relay_is_handed_ciphertext_and_never_the_key() {
        let relay = RecordingRelay::new();
        let random = CountingRandomness::new();
        let published = publish(
            &[entry("shot.png", b"PLAINTEXT-MARKER")],
            &settings(1 << 20),
            &relay,
            &random,
        )
        .unwrap_or_else(|error| panic!("{error}"));

        let stored = relay.stored_bytes();
        assert!(
            !stored
                .windows(b"PLAINTEXT-MARKER".len())
                .any(|window| window == b"PLAINTEXT-MARKER"),
            "the plaintext reached the relay"
        );
        let key = published.token().key().encoded();
        assert!(
            !String::from_utf8_lossy(&stored).contains(&key),
            "the key reached the relay"
        );
        assert!(
            !relay
                .recorded_calls()
                .iter()
                .any(|call| call.contains(&key))
        );
    }

    #[test]
    fn the_token_opens_what_was_published() {
        let relay = RecordingRelay::new();
        let random = CountingRandomness::new();
        let published = publish(
            &[entry("shot.png", b"pixels")],
            &settings(1 << 20),
            &relay,
            &random,
        )
        .unwrap_or_else(|error| panic!("{error}"));

        let frame = crypto::open(&relay.stored_bytes(), published.token().key())
            .unwrap_or_else(|error| panic!("{error}"));
        let decoded = bundle::decode(&frame).unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(decoded.len(), 1);
        assert_eq!(decoded[0].data(), b"pixels");
        assert_eq!(decoded[0].name().as_str(), "shot.png");
    }

    /// Two sends of the same bytes must not produce the same ciphertext, or the
    /// relay operator learns that the user sent the same screenshot twice.
    #[test]
    fn two_publishes_of_identical_bytes_differ() {
        let random = CountingRandomness::new();
        let first = RecordingRelay::new();
        let second = RecordingRelay::new();
        let a = publish(
            &[entry("s.png", b"same")],
            &settings(1 << 20),
            &first,
            &random,
        )
        .unwrap_or_else(|error| panic!("{error}"));
        let b = publish(
            &[entry("s.png", b"same")],
            &settings(1 << 20),
            &second,
            &random,
        )
        .unwrap_or_else(|error| panic!("{error}"));
        assert_ne!(first.stored_bytes(), second.stored_bytes());
        assert_ne!(a.token().expose(), b.token().expose());
        assert_ne!(a.token().key().as_bytes(), b.token().key().as_bytes());
    }

    #[test]
    fn an_object_over_the_ceiling_is_refused_before_anything_is_uploaded() {
        let relay = RecordingRelay::new();
        let error = publish(
            &[entry("big.png", &vec![0_u8; 4096])],
            &settings(1024),
            &relay,
            &CountingRandomness::new(),
        )
        .unwrap_err();
        assert_eq!(error.kind(), ErrorKind::LimitExceeded);
        assert!(
            relay.recorded_calls().is_empty(),
            "the relay was contacted anyway"
        );
    }

    #[test]
    fn an_unavailable_random_source_aborts_rather_than_falling_back() {
        let relay = RecordingRelay::new();
        let error = publish(
            &[entry("shot.png", b"x")],
            &settings(1 << 20),
            &relay,
            &FailingRandomness,
        )
        .unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Internal);
        assert!(relay.recorded_calls().is_empty());
    }

    #[test]
    fn nothing_is_published_when_there_is_nothing_to_send() {
        let relay = RecordingRelay::new();
        assert!(publish(&[], &settings(1 << 20), &relay, &CountingRandomness::new()).is_err());
        assert!(relay.recorded_calls().is_empty());
    }
}
