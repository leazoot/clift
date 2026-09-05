//! The one short string that travels through the user's terminal.
//!
//! ```text
//! clift://v1/<object-id>#<key-material>
//! ```
//!
//! The shape is a URL on purpose: the fragment after `#` is the part that, by
//! long convention, is never sent to a server. That convention is the whole
//! design here. The object id goes to the relay; the key material never does,
//! and [`Token::relay_half`] is the only thing a relay client is ever handed.
//!
//! Two rules about this type that the rest of the code depends on:
//!
//! - `Debug` redacts the key. A token in a log or a panic message is a token in
//!   somebody's shell history, and the whole point of a single-use object is
//!   defeated by the copy that outlives it.
//! - There is no `Display`. Rendering the complete token is done through
//!   [`Token::expose`], which is a name a reviewer can grep for.

use crate::error::{CliftError, ErrorKind, Remedy, Stage};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use std::fmt;

/// The scheme and version prefix every token starts with.
pub const TOKEN_PREFIX: &str = "clift://v1/";

/// Bytes in an object id. 128 bits: the relay's namespace must be large enough
/// that guessing an id is not a way to find somebody else's ciphertext.
pub const OBJECT_ID_BYTES: usize = 16;

/// Bytes in an object key. XChaCha20-Poly1305 takes 256 bits.
pub const SEAL_KEY_BYTES: usize = 32;

/// The relay's name for one stored ciphertext.
///
/// Not secret: it is in the request line of every fetch, and a relay operator
/// necessarily sees it. What it is not is *guessable*, which is why it is 128
/// bits from a CSPRNG rather than a counter.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ObjectId([u8; OBJECT_ID_BYTES]);

impl ObjectId {
    /// # Errors
    /// Never fails; the signature is uniform with the parsing constructor.
    #[must_use]
    pub const fn from_bytes(bytes: [u8; OBJECT_ID_BYTES]) -> Self {
        Self(bytes)
    }

    /// Parses the base64url form used in a token and in a request path.
    ///
    /// # Errors
    /// Fails when the text is not base64url, or decodes to the wrong length.
    pub fn parse(text: &str) -> Result<Self, CliftError> {
        let decoded = URL_SAFE_NO_PAD.decode(text.as_bytes()).map_err(|error| {
            malformed("the object id is not valid base64url").with_source(error)
        })?;
        // The value `try_into` hands back on failure is the decoded input
        // itself, which for the sibling constructor below is the key. Neither
        // is put in a cause chain -- there is no information in "here are the
        // bytes again" that a person debugging wants, and there is a great deal
        // in it that must not reach a log.
        let Ok(bytes): Result<[u8; OBJECT_ID_BYTES], _> = decoded.try_into() else {
            return Err(malformed("the object id is the wrong length"));
        };
        Ok(Self(bytes))
    }

    #[must_use]
    pub fn encoded(&self) -> String {
        URL_SAFE_NO_PAD.encode(self.0)
    }

    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; OBJECT_ID_BYTES] {
        &self.0
    }
}

/// An object id is safe to print, and printing it is how a relay logs a
/// request. The key material is a different type for exactly that reason.
impl fmt::Display for ObjectId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.encoded())
    }
}

impl fmt::Debug for ObjectId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ObjectId({})", self.encoded())
    }
}

/// The symmetric key for one object, and nothing else.
///
/// A fresh one per attachment. Reuse would be the one mistake this design
/// cannot survive, so there is no constructor that derives a key from anything
/// -- it comes from the operating system's random source or from a token the
/// user pasted, and from nowhere else.
#[derive(Clone, PartialEq, Eq)]
pub struct SealKey([u8; SEAL_KEY_BYTES]);

impl SealKey {
    #[must_use]
    pub const fn from_bytes(bytes: [u8; SEAL_KEY_BYTES]) -> Self {
        Self(bytes)
    }

    /// # Errors
    /// Fails when the text is not base64url, or decodes to the wrong length.
    pub fn parse(text: &str) -> Result<Self, CliftError> {
        let decoded = URL_SAFE_NO_PAD.decode(text.as_bytes()).map_err(|error| {
            malformed("the key material is not valid base64url").with_source(error)
        })?;
        // See [`ObjectId::parse`]: the discarded value is the key material.
        let Ok(bytes): Result<[u8; SEAL_KEY_BYTES], _> = decoded.try_into() else {
            return Err(malformed("the key material is the wrong length"));
        };
        Ok(Self(bytes))
    }

    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; SEAL_KEY_BYTES] {
        &self.0
    }

    /// The only way to turn a key back into text. Used once, when the token is
    /// rendered for the user to paste.
    #[must_use]
    pub fn encoded(&self) -> String {
        URL_SAFE_NO_PAD.encode(self.0)
    }
}

/// Never the key. A `{:?}` in a log line must not be the thing that leaks it.
impl fmt::Debug for SealKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SealKey(<redacted>)")
    }
}

/// Overwrites the key before the memory is returned.
///
/// This is not a defence against an attacker who can read the process's memory
/// -- nothing at this layer is. It shortens the window in which a copy sits in
/// a freed allocation, which is worth the three lines it costs. `black_box` is
/// how that is done without `unsafe`: the zeroed array is observed afterwards,
/// so the compiler may not decide the stores were dead.
impl Drop for SealKey {
    fn drop(&mut self) {
        self.0.fill(0);
        let _ = std::hint::black_box(&self.0);
    }
}

/// An object id and the key that opens it.
#[derive(Clone, PartialEq, Eq)]
pub struct Token {
    id: ObjectId,
    key: SealKey,
}

impl Token {
    #[must_use]
    pub const fn new(id: ObjectId, key: SealKey) -> Self {
        Self { id, key }
    }

    #[must_use]
    pub const fn id(&self) -> &ObjectId {
        &self.id
    }

    #[must_use]
    pub const fn key(&self) -> &SealKey {
        &self.key
    }

    /// Parses what the user pasted.
    ///
    /// Deliberately strict. A token that is nearly right is not treated
    /// generously: it is a 60-character string that a machine produced, so the
    /// only reason for it to be slightly wrong is that it was truncated or
    /// mangled in transit, and guessing at the missing part would be guessing
    /// at key material.
    ///
    /// # Errors
    /// Returns [`ErrorKind::TokenUnusable`] for a wrong scheme, a wrong
    /// version, a missing half, or either half being the wrong length.
    pub fn parse(text: &str) -> Result<Self, CliftError> {
        let text = text.trim();
        let Some(rest) = text.strip_prefix(TOKEN_PREFIX) else {
            // A different version is worth its own message: it tells the user
            // which side to upgrade, which "malformed token" does not.
            if let Some(other) = text.strip_prefix("clift://") {
                let version = other.split('/').next().unwrap_or("");
                return Err(malformed(format!(
                    "this token is version {version:?}, and this build only understands v1"
                ))
                .with_remedy(Remedy::new(
                    "Check which build is on each side:",
                    "clift --version",
                )));
            }
            return Err(malformed(format!(
                "a token starts with {TOKEN_PREFIX:?}, and this one does not"
            )));
        };

        let Some((id, key)) = rest.split_once('#') else {
            return Err(malformed(
                "the token has no key material; it was probably cut short when it was copied",
            ));
        };
        if id.is_empty() || key.is_empty() {
            return Err(malformed("the token has an empty half"));
        }

        Ok(Self {
            id: ObjectId::parse(id)?,
            key: SealKey::parse(key)?,
        })
    }

    /// The complete token, for the user to paste.
    ///
    /// The name says what it does. Every call site is a place a reviewer should
    /// look at, and there are three: rendering the instruction, `--json`, and
    /// `clift copy`, which prints the token by itself for someone to select off
    /// a terminal. Anything that already holds a `Token` and only needs to act
    /// on it must pass the `Token`, not a string made here.
    #[must_use]
    pub fn expose(&self) -> String {
        format!("{TOKEN_PREFIX}{}#{}", self.id.encoded(), self.key.encoded())
    }

    /// The token with the key replaced by a placeholder, for anything that gets
    /// written down.
    #[must_use]
    pub fn redacted(&self) -> String {
        format!("{TOKEN_PREFIX}{}#<redacted>", self.id.encoded())
    }
}

impl fmt::Debug for Token {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Token({})", self.redacted())
    }
}

fn malformed(message: impl Into<String>) -> CliftError {
    CliftError::new(Stage::Relay, ErrorKind::TokenUnusable, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Token {
        Token::new(
            ObjectId::from_bytes([7; OBJECT_ID_BYTES]),
            SealKey::from_bytes([9; SEAL_KEY_BYTES]),
        )
    }

    #[test]
    fn a_token_round_trips_through_its_text_form() {
        let token = sample();
        let text = token.expose();
        assert!(text.starts_with("clift://v1/"));
        assert!(text.contains('#'));
        let parsed = Token::parse(&text).unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(parsed.id(), token.id());
        assert_eq!(parsed.key().as_bytes(), token.key().as_bytes());
    }

    #[test]
    fn surrounding_whitespace_is_tolerated_because_a_terminal_adds_it() {
        let text = format!("  {}\n", sample().expose());
        assert!(Token::parse(&text).is_ok());
    }

    #[test]
    fn the_key_is_never_in_a_debug_rendering() {
        let token = sample();
        let key = token.key().encoded();
        let debug = format!("{token:?}");
        assert!(!debug.contains(&key), "{debug}");
        assert!(debug.contains("<redacted>"), "{debug}");
        assert!(!format!("{:?}", token.key()).contains(&key));
        assert!(!token.redacted().contains(&key));
    }

    #[test]
    fn a_future_version_is_refused_by_name_rather_than_reinterpreted() {
        let error = Token::parse("clift://v2/AAAAAAAAAAAAAAAAAAAAAA#AAAA").unwrap_err();
        assert_eq!(error.kind(), ErrorKind::TokenUnusable);
        assert!(error.message().contains("v2"), "{}", error.message());
    }

    #[test]
    fn every_way_of_being_wrong_is_refused() {
        let good = sample().expose();
        let cases = [
            ("", "empty"),
            ("clift", "no scheme"),
            ("http://example.com/x#y", "wrong scheme"),
            ("clift://v1/onlyanid", "no fragment"),
            ("clift://v1/#key", "empty id"),
            ("clift://v1/id#", "empty key"),
            ("clift://v1/AAAA#AAAA", "both halves too short"),
        ];
        for (text, why) in cases {
            assert!(Token::parse(text).is_err(), "accepted {why}: {text:?}");
        }
        // Truncation is the realistic failure, and it must not be tolerated.
        for cut in 1..12 {
            let truncated = &good[..good.len() - cut];
            assert!(Token::parse(truncated).is_err(), "accepted {truncated:?}");
        }
    }

    #[test]
    fn an_object_id_round_trips_and_rejects_the_wrong_length() {
        let id = ObjectId::from_bytes([3; OBJECT_ID_BYTES]);
        assert_eq!(
            ObjectId::parse(&id.encoded()).unwrap_or_else(|e| panic!("{e}")),
            id
        );
        assert!(ObjectId::parse("AAAA").is_err());
        assert!(ObjectId::parse("not base64!!").is_err());
    }
}
