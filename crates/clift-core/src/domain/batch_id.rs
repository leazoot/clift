//! Identifier of one upload batch, and of the remote directory holding it.

use super::DomainError;
use std::fmt;

/// An unpredictable batch identifier.
///
/// The directory name is the only thing keeping two batches that contain a file
/// of the same name from overwriting each other, and it is also what stops
/// another user on the remote host from guessing a path. It must therefore come
/// from the operating system CSPRNG: a timestamp, PID, counter or content hash
/// is either predictable or leaks a fingerprint of the content.
///
/// Generation lives behind the `IdSource` port; this type only carries the
/// bytes and enforces that enough of them were supplied.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BatchId(String);

/// 128 bits. Guessing a directory is then infeasible even for someone who can
/// list the parent's mtimes.
pub const BYTES: usize = 16;

impl BatchId {
    /// Builds an identifier from bytes that **must** come from the OS CSPRNG.
    ///
    /// # Errors
    /// Rejects an all-zero buffer, which in practice means an uninitialised
    /// array rather than an astronomically unlikely draw.
    pub fn from_random_bytes(bytes: [u8; BYTES]) -> Result<Self, DomainError> {
        if bytes.iter().all(|byte| *byte == 0) {
            return Err(DomainError::new(
                "batch id",
                "all-zero bytes indicate an uninitialised buffer, not a random draw",
            ));
        }
        let mut hex = String::with_capacity(BYTES * 2);
        for byte in bytes {
            // Lowercase hex keeps the directory name portable across remote
            // filesystems that fold case.
            hex.push(char::from_digit(u32::from(byte >> 4), 16).unwrap_or('0'));
            hex.push(char::from_digit(u32::from(byte & 0x0f), 16).unwrap_or('0'));
        }
        Ok(Self(hex))
    }

    /// Reads back an identifier Clift itself produced.
    ///
    /// Used for the session nonce, which needs the same unpredictable 128 bits
    /// and, unlike a batch directory, has to survive a round trip through a
    /// file. The check is deliberately strict: anything that is not exactly the
    /// hex this type emits is not something this type produced.
    ///
    /// # Errors
    /// Rejects a value of the wrong length, anything that is not lowercase hex,
    /// and the all-zero value.
    pub fn from_hex(value: &str) -> Result<Self, DomainError> {
        let subject = "batch id";
        if value.len() != BYTES * 2 {
            return Err(DomainError::new(subject, "must be 32 hexadecimal digits"));
        }
        if !value
            .chars()
            .all(|character| character.is_ascii_digit() || ('a'..='f').contains(&character))
        {
            return Err(DomainError::new(subject, "must be lowercase hexadecimal"));
        }
        if value.chars().all(|character| character == '0') {
            return Err(DomainError::new(
                subject,
                "all-zero bytes indicate an uninitialised buffer, not a random draw",
            ));
        }
        Ok(Self(value.to_string()))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for BatchId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_as_lowercase_hex_of_the_full_buffer() {
        let id = BatchId::from_random_bytes([
            0x00, 0x01, 0x0f, 0x10, 0xa0, 0xff, 0x7b, 0x2c, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88,
            0x99, 0xaa,
        ])
        .unwrap();
        assert_eq!(id.as_str(), "00010f10a0ff7b2c33445566778899aa");
        assert_eq!(id.as_str().len(), BYTES * 2);
        assert!(id.as_str().chars().all(|c| c.is_ascii_hexdigit()));
        assert!(!id.as_str().chars().any(|c| c.is_ascii_uppercase()));
    }

    #[test]
    fn rejects_an_uninitialised_buffer() {
        assert!(BatchId::from_random_bytes([0; BYTES]).is_err());
    }

    #[test]
    fn distinct_input_yields_distinct_ids() {
        let mut a = [7u8; BYTES];
        let b = [7u8; BYTES];
        a[BYTES - 1] = 8;
        assert_ne!(
            BatchId::from_random_bytes(a).unwrap(),
            BatchId::from_random_bytes(b).unwrap()
        );
    }

    #[test]
    fn identifier_is_a_valid_single_path_component() {
        let id = BatchId::from_random_bytes([1; BYTES]).unwrap();
        assert!(super::super::SafeFileName::new(id.as_str()).is_ok());
    }
}
