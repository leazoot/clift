//! Entry point for the staging security suite.
//!
//! The suite itself lives at `tests/security/staging.rs`, beside the other
//! cross-cutting test material, and is compiled into this crate because that is
//! where the real transport is.

#[path = "../../../tests/security/staging.rs"]
mod staging;
