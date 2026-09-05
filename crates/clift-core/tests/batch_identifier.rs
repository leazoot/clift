//! The batch identifier's contract, against a real CSPRNG.
//!
//! `BatchId` cannot check where its bytes came from, so this checks the thing
//! that can be checked: that identifiers drawn from the operating system's
//! random source are unique and carry the entropy the directory name depends
//! on. The specification rules out a timestamp, a PID, a counter or a content hash --
//! each of those is either guessable or leaks something about the content.

// A panic here is a test failure, not a user-facing crash; see clippy.toml.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use clift_core::domain::{BATCH_ID_BYTES, BatchId};
use std::collections::HashSet;

fn from_system_random() -> BatchId {
    let mut bytes = [0u8; BATCH_ID_BYTES];
    getrandom::fill(&mut bytes).expect("the operating system random source must be available");
    BatchId::from_random_bytes(bytes).expect("a CSPRNG draw is not all zero")
}

#[test]
fn ten_thousand_identifiers_from_the_system_source_are_all_distinct() {
    const DRAWS: usize = 10_000;
    let seen: HashSet<String> = (0..DRAWS)
        .map(|_| from_system_random().as_str().to_string())
        .collect();
    assert_eq!(seen.len(), DRAWS, "the random source repeated itself");
}

/// 128 bits is the floor, and it has to be visible in the rendered name: a
/// shorter identifier would be guessable by someone who can list the parent.
#[test]
fn an_identifier_carries_a_full_128_bits() {
    assert_eq!(BATCH_ID_BYTES, 16);
    for _ in 0..100 {
        let id = from_system_random();
        assert_eq!(id.as_str().len(), BATCH_ID_BYTES * 2);
        assert!(id.as_str().chars().all(|c| c.is_ascii_hexdigit()));
    }
}

/// A weak source would show up as a stuck bit long before it showed up as a
/// collision. Across enough draws every bit position must take both values.
#[test]
fn no_bit_of_the_identifier_is_stuck() {
    let mut ones = [0u32; BATCH_ID_BYTES * 8];
    const DRAWS: u32 = 500;

    for _ in 0..DRAWS {
        let mut bytes = [0u8; BATCH_ID_BYTES];
        getrandom::fill(&mut bytes).expect("the system random source must be available");
        for (index, byte) in bytes.iter().enumerate() {
            for bit in 0..8 {
                if byte & (1 << bit) != 0 {
                    ones[index * 8 + bit] += 1;
                }
            }
        }
    }

    for (position, count) in ones.iter().enumerate() {
        assert!(
            *count > 0 && *count < DRAWS,
            "bit {position} was constant across {DRAWS} draws"
        );
    }
}

/// The type refuses a buffer that was never filled, which is what an
/// uninitialised array or a silently failing source looks like.
#[test]
fn an_unfilled_buffer_is_refused() {
    assert!(BatchId::from_random_bytes([0; BATCH_ID_BYTES]).is_err());
}
