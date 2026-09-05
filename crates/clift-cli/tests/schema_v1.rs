//! Entry point for the JSON contract tests.
//!
//! The suite lives at `tests/schema/json_v1.rs`, beside the other cross-cutting
//! test material, and is compiled here because this is the crate that produces
//! the documents.

#[path = "../../../tests/schema/json_v1.rs"]
mod json_v1;
