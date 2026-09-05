//! Sealing an attachment so that the relay carries something it cannot read.
//!
//! One algorithm, named in the envelope so a future one can be added without
//! guessing: XChaCha20-Poly1305. It was chosen over AES-256-GCM for two
//! reasons that matter to this particular program.
//!
//! - **The nonce.** XChaCha20's nonce is 192 bits, so a random one per object
//!   is safe with no counter to persist. AES-GCM's is 96 bits, which is small
//!   enough that "just use random" needs an argument about how many objects
//!   will ever exist. Clift has nowhere to keep a counter -- the local config
//!   is the only persistent state and putting a cryptographic counter in it
//!   would be a new class of bug -- so the algorithm that does not need one is
//!   the right one.
//! - **The remote side.** `clift fetch` runs on whatever the user's VPS is.
//!   ChaCha20 is constant-time in software on every architecture; AES without
//!   hardware support is not, and Clift cannot know what its remote is.
//!
//! The envelope is deliberately not a self-describing format. It is fixed
//! width, parsed by index, and every field is checked before use.
//!
//! ```text
//! 0..7    magic     b"CLIFTv1"
//! 7       algorithm 1 = XChaCha20-Poly1305
//! 8..32   nonce     24 bytes
//! 32..    ciphertext, with the 16-byte Poly1305 tag appended
//! ```

use crate::error::{CliftError, ErrorKind, Stage};
use crate::universal::token::SealKey;
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{Key, XChaCha20Poly1305, XNonce};

/// The envelope's fixed header.
const MAGIC: [u8; 7] = *b"CLIFTv1";

/// XChaCha20-Poly1305. The only value this build writes or accepts.
const ALGORITHM_XCHACHA20_POLY1305: u8 = 1;

/// Bytes in an XChaCha20 nonce.
pub const NONCE_BYTES: usize = 24;

/// Bytes of Poly1305 tag the AEAD appends.
const TAG_BYTES: usize = 16;

const HEADER_BYTES: usize = MAGIC.len() + 1 + NONCE_BYTES;

/// Associated data, so the envelope's own header is authenticated along with
/// the payload. An attacker who flips the algorithm byte gets a tag failure
/// rather than a different code path.
fn associated_data(nonce: &[u8; NONCE_BYTES]) -> Vec<u8> {
    let mut aad = Vec::with_capacity(HEADER_BYTES + 16);
    aad.extend_from_slice(b"clift/v1/object");
    aad.extend_from_slice(&MAGIC);
    aad.push(ALGORITHM_XCHACHA20_POLY1305);
    aad.extend_from_slice(nonce);
    aad
}

/// The smallest sealed object that could possibly be valid: header plus a tag
/// over an empty payload.
pub const MIN_SEALED_BYTES: usize = HEADER_BYTES + TAG_BYTES;

/// How many bytes sealing `plaintext_len` bytes will produce.
///
/// Used to refuse an attachment against the relay's limit *before* encrypting
/// it, rather than after.
#[must_use]
pub const fn sealed_len(plaintext_len: usize) -> usize {
    HEADER_BYTES + plaintext_len + TAG_BYTES
}

/// Encrypts `plaintext` under `key` with `nonce`.
///
/// The nonce is a parameter rather than drawn here, because `clift-core`
/// performs no IO and randomness is a port. That also makes the whole of this
/// module deterministic, and therefore testable against fixed vectors.
///
/// # Errors
/// Fails only if the AEAD itself refuses, which in practice means the payload
/// is larger than the construction can carry.
pub fn seal(
    plaintext: &[u8],
    key: &SealKey,
    nonce: &[u8; NONCE_BYTES],
) -> Result<Vec<u8>, CliftError> {
    let cipher = XChaCha20Poly1305::new(&Key::from(*key.as_bytes()));
    let aad = associated_data(nonce);
    let ciphertext = cipher
        .encrypt(
            &XNonce::from(*nonce),
            Payload {
                msg: plaintext,
                aad: &aad,
            },
        )
        .map_err(|error| {
            // The AEAD's error type carries no detail by design, so there is no
            // cause chain to preserve here -- the message is the whole of it.
            CliftError::new(
                Stage::Relay,
                ErrorKind::Internal,
                format!("the attachment could not be encrypted: {error}"),
            )
        })?;

    let mut sealed = Vec::with_capacity(sealed_len(plaintext.len()));
    sealed.extend_from_slice(&MAGIC);
    sealed.push(ALGORITHM_XCHACHA20_POLY1305);
    sealed.extend_from_slice(nonce);
    sealed.extend_from_slice(&ciphertext);
    Ok(sealed)
}

/// Decrypts a sealed object.
///
/// Every failure below is the same kind of failure to the caller
/// ([`ErrorKind::IntegrityFailure`]) and they are worth distinguishing only in
/// the message: a truncated download, a tampered byte and the wrong key are
/// three different things to a person debugging and one thing to the security
/// model.
///
/// # Errors
/// Fails when the envelope is too short, has the wrong magic, names an
/// algorithm this build does not implement, or does not authenticate.
pub fn open(sealed: &[u8], key: &SealKey) -> Result<Vec<u8>, CliftError> {
    if sealed.len() < MIN_SEALED_BYTES {
        return Err(integrity(format!(
            "the downloaded object is {} bytes, which is shorter than an empty one",
            sealed.len()
        )));
    }
    if sealed[..MAGIC.len()] != MAGIC {
        return Err(integrity(
            "the downloaded object is not a Clift envelope; the relay returned something else",
        ));
    }
    let algorithm = sealed[MAGIC.len()];
    if algorithm != ALGORITHM_XCHACHA20_POLY1305 {
        return Err(integrity(format!(
            "the object was sealed with algorithm {algorithm}, and this build only implements 1"
        )));
    }

    let mut nonce = [0_u8; NONCE_BYTES];
    nonce.copy_from_slice(&sealed[MAGIC.len() + 1..HEADER_BYTES]);

    let cipher = XChaCha20Poly1305::new(&Key::from(*key.as_bytes()));
    // The AEAD returns one opaque error for every reason a tag can fail, and
    // that is correct behaviour rather than a limitation: telling a caller
    // *why* authentication failed is how padding oracles get built. So there is
    // no cause chain to preserve here -- there is no cause to have.
    let Ok(plaintext) = cipher.decrypt(
        &XNonce::from(nonce),
        Payload {
            msg: &sealed[HEADER_BYTES..],
            aad: &associated_data(&nonce),
        },
    ) else {
        return Err(integrity(
            "the object did not decrypt: the key does not match, or the bytes were altered",
        ));
    };
    Ok(plaintext)
}

fn integrity(message: impl Into<String>) -> CliftError {
    CliftError::new(Stage::Relay, ErrorKind::IntegrityFailure, message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::universal::token::SEAL_KEY_BYTES;

    fn key(fill: u8) -> SealKey {
        SealKey::from_bytes([fill; SEAL_KEY_BYTES])
    }

    const NONCE: [u8; NONCE_BYTES] = [4; NONCE_BYTES];

    #[test]
    fn a_sealed_payload_opens_again() {
        let plaintext = b"the quick brown fox".to_vec();
        let sealed = seal(&plaintext, &key(1), &NONCE).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(sealed.len(), sealed_len(plaintext.len()));
        assert_eq!(
            open(&sealed, &key(1)).unwrap_or_else(|e| panic!("{e}")),
            plaintext
        );
    }

    #[test]
    fn the_plaintext_is_not_in_the_ciphertext() {
        let plaintext = b"SECRET-MARKER-9f3c".to_vec();
        let sealed = seal(&plaintext, &key(1), &NONCE).unwrap_or_else(|e| panic!("{e}"));
        assert!(
            !sealed
                .windows(plaintext.len())
                .any(|window| window == plaintext.as_slice())
        );
    }

    #[test]
    fn the_wrong_key_does_not_open_it() {
        let sealed = seal(b"payload", &key(1), &NONCE).unwrap_or_else(|e| panic!("{e}"));
        let error = open(&sealed, &key(2)).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::IntegrityFailure);
    }

    /// Every single-byte change anywhere in the envelope must be caught. This
    /// is the assertion that the header is authenticated too, not only the
    /// payload -- flipping the algorithm byte or the nonce has to fail as
    /// surely as flipping a ciphertext byte.
    #[test]
    fn no_single_byte_of_the_envelope_can_be_altered_unnoticed() {
        let sealed = seal(b"payload for tamper test", &key(1), &NONCE)
            .unwrap_or_else(|error| panic!("{error}"));
        for index in 0..sealed.len() {
            let mut tampered = sealed.clone();
            tampered[index] ^= 0x01;
            assert!(
                open(&tampered, &key(1)).is_err(),
                "byte {index} could be altered without detection"
            );
        }
    }

    #[test]
    fn truncation_is_caught_at_every_length() {
        let sealed = seal(b"payload", &key(1), &NONCE).unwrap_or_else(|e| panic!("{e}"));
        for length in 0..sealed.len() {
            assert!(
                open(&sealed[..length], &key(1)).is_err(),
                "a {length}-byte prefix was accepted"
            );
        }
    }

    #[test]
    fn something_that_is_not_an_envelope_is_refused_before_the_aead_sees_it() {
        let junk = vec![0_u8; MIN_SEALED_BYTES + 8];
        let error = open(&junk, &key(1)).unwrap_err();
        assert!(
            error.message().contains("not a Clift envelope"),
            "{}",
            error.message()
        );
    }

    #[test]
    fn a_different_nonce_gives_a_different_ciphertext() {
        let first = seal(b"same", &key(1), &[1; NONCE_BYTES]).unwrap_or_else(|e| panic!("{e}"));
        let second = seal(b"same", &key(1), &[2; NONCE_BYTES]).unwrap_or_else(|e| panic!("{e}"));
        assert_ne!(first, second);
    }

    #[test]
    fn an_empty_payload_still_round_trips() {
        let sealed = seal(b"", &key(1), &NONCE).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(sealed.len(), MIN_SEALED_BYTES);
        assert!(
            open(&sealed, &key(1))
                .unwrap_or_else(|e| panic!("{e}"))
                .is_empty()
        );
    }
}
