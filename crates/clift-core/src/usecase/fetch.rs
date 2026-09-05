//! Universal Mode, receiving half: redeem a token on the host that got it.
//!
//! This runs on the VPS, started by the agent. The order:
//!
//! 1. **parse the token** -- before the network is touched, so a mangled paste
//!    costs nothing and says so;
//! 2. **retrieve** -- by object id alone. The key does not leave this process;
//! 3. **open** -- authenticate and decrypt locally;
//! 4. **decode** -- the frame, with every length checked;
//! 5. **write** -- into a fresh batch directory, atomically, one file at a time.
//!
//! Step 5 is the last thing that happens, and nothing before it produces
//! output. That is the same atomicity promise `send` makes applied to
//! the other direction: a fetch that fails prints no path, so an agent is never
//! handed the name of a file that is not there.

use crate::error::CliftError;
use crate::ports::{Clock, IdSource, Relay};
use crate::staging::{WrittenBatch, write_batch};
use crate::universal::{Token, bundle, crypto};
use std::path::Path;

/// What one redeemed token produced.
#[derive(Debug)]
pub struct Fetched {
    batch: WrittenBatch,
}

impl Fetched {
    #[must_use]
    pub const fn batch(&self) -> &WrittenBatch {
        &self.batch
    }
}

/// Redeems `token` and writes what it holds under `inbox_root`.
///
/// # Errors
/// Returns [`crate::error::ErrorKind::TokenUnusable`] when the object is gone,
/// [`crate::error::ErrorKind::RelayUnavailable`] when the relay cannot be
/// reached, [`crate::error::ErrorKind::IntegrityFailure`] when what arrives
/// does not authenticate or does not decode, and a staging error when the file
/// cannot be written.
pub fn fetch(
    token: &Token,
    inbox_root: &Path,
    relay: &dyn Relay,
    clock: &dyn Clock,
    ids: &dyn IdSource,
) -> Result<Fetched, CliftError> {
    let sealed = relay.retrieve(token.id())?;
    let frame = crypto::open(&sealed, token.key())?;
    let entries = bundle::decode(&frame)?;
    let batch = write_batch(inbox_root, &entries, clock, ids)?;
    Ok(Fetched { batch })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::SafeFileName;
    use crate::error::ErrorKind;
    use crate::testing::{CountingRandomness, FakeClock, FakeIdSource, RecordingRelay};
    use crate::universal::{BundleEntry, DEFAULT_TTL, RelaySettings};
    use crate::usecase::publish::publish;
    use std::fs;
    use std::path::PathBuf;

    struct Scratch(PathBuf);

    impl Scratch {
        fn new(tag: &str) -> Self {
            let mut bytes = [0_u8; 8];
            getrandom::fill(&mut bytes).unwrap_or_else(|error| panic!("{error}"));
            let path = std::env::temp_dir()
                .join(format!("clift-fetch-{tag}-{}", u64::from_be_bytes(bytes)));
            fs::create_dir_all(&path).unwrap_or_else(|error| panic!("{error}"));
            Self(path)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn entry(name: &str, data: &[u8]) -> BundleEntry {
        BundleEntry::new(
            SafeFileName::new(name).unwrap_or_else(|error| panic!("{error}")),
            "image/png",
            data.to_vec(),
        )
        .unwrap_or_else(|error| panic!("{error}"))
    }

    fn settings() -> RelaySettings {
        RelaySettings::new("https://relay.example.com", 1 << 20, DEFAULT_TTL)
            .unwrap_or_else(|error| panic!("{error}"))
    }

    /// The whole round trip through the ports, with no host and no target
    /// anywhere in it. This is the assertion that Universal Mode does not need
    /// one.
    #[test]
    fn a_published_object_comes_back_out_the_other_side() {
        let relay = RecordingRelay::new();
        let published = publish(
            &[entry("shot.png", b"pixels"), entry("notes.png", b"more")],
            &settings(),
            &relay,
            &CountingRandomness::new(),
        )
        .unwrap_or_else(|error| panic!("{error}"));

        let scratch = Scratch::new("roundtrip");
        let fetched = fetch(
            published.token(),
            &scratch.0.join("inbox"),
            &relay,
            &FakeClock::at_unix_seconds(1_788_093_240),
            &FakeIdSource::starting_at(1),
        )
        .unwrap_or_else(|error| panic!("{error}"));

        let paths = fetched.batch().paths();
        assert_eq!(paths.len(), 2);
        assert_eq!(fs::read(paths[0].as_str()).unwrap_or_default(), b"pixels");
        assert_eq!(fs::read(paths[1].as_str()).unwrap_or_default(), b"more");
        assert!(paths[0].as_str().ends_with("/shot.png"), "{}", paths[0]);
    }

    /// An object is single use. The second attempt must fail, and it
    /// must fail as "gone", not as "broken".
    #[test]
    fn the_same_token_cannot_be_redeemed_twice() {
        let relay = RecordingRelay::new();
        let published = publish(
            &[entry("shot.png", b"pixels")],
            &settings(),
            &relay,
            &CountingRandomness::new(),
        )
        .unwrap_or_else(|error| panic!("{error}"));

        let scratch = Scratch::new("single-use");
        let root = scratch.0.join("inbox");
        let clock = FakeClock::at_unix_seconds(1_788_093_240);
        let ids = FakeIdSource::starting_at(1);

        fetch(published.token(), &root, &relay, &clock, &ids)
            .unwrap_or_else(|error| panic!("{error}"));
        let error = fetch(published.token(), &root, &relay, &clock, &ids).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::TokenUnusable);
    }

    #[test]
    fn a_revoked_object_can_no_longer_be_fetched() {
        let relay = RecordingRelay::new();
        let published = publish(
            &[entry("shot.png", b"pixels")],
            &settings(),
            &relay,
            &CountingRandomness::new(),
        )
        .unwrap_or_else(|error| panic!("{error}"));

        relay
            .revoke(published.token().id())
            .unwrap_or_else(|error| panic!("{error}"));

        let scratch = Scratch::new("revoked");
        let error = fetch(
            published.token(),
            &scratch.0.join("inbox"),
            &relay,
            &FakeClock::at_unix_seconds(1_788_093_240),
            &FakeIdSource::starting_at(1),
        )
        .unwrap_err();
        assert_eq!(error.kind(), ErrorKind::TokenUnusable);
    }

    /// A relay that alters the ciphertext must not be able to make the fetching
    /// side write anything at all.
    #[test]
    fn a_tampered_object_writes_nothing() {
        let relay = RecordingRelay::new();
        let published = publish(
            &[entry("shot.png", b"pixels")],
            &settings(),
            &relay,
            &CountingRandomness::new(),
        )
        .unwrap_or_else(|error| panic!("{error}"));
        relay.tamper_with_stored_object();

        let scratch = Scratch::new("tampered");
        let root = scratch.0.join("inbox");
        let error = fetch(
            published.token(),
            &root,
            &relay,
            &FakeClock::at_unix_seconds(1_788_093_240),
            &FakeIdSource::starting_at(1),
        )
        .unwrap_err();
        assert_eq!(error.kind(), ErrorKind::IntegrityFailure);
        assert!(!root.exists(), "a failed fetch created the inbox anyway");
    }

    #[test]
    fn a_relay_that_is_down_is_reported_as_such_and_writes_nothing() {
        let relay = RecordingRelay::unavailable();
        let scratch = Scratch::new("down");
        let root = scratch.0.join("inbox");
        let token = Token::parse(
            "clift://v1/AAECAwQFBgcICQoLDA0ODw#AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8",
        )
        .unwrap_or_else(|error| panic!("{error}"));

        let error = fetch(
            &token,
            &root,
            &relay,
            &FakeClock::at_unix_seconds(1_788_093_240),
            &FakeIdSource::starting_at(1),
        )
        .unwrap_err();
        assert_eq!(error.kind(), ErrorKind::RelayUnavailable);
        assert!(!root.exists());
    }
}
