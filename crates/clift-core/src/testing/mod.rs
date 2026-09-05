//! In-memory port implementations for unit tests.
//!
//! Behind the `testing` feature, which nothing in the shipped binary enables.
//! The specification forbids mock end-to-end tests, so these must never be reachable
//! from `clift-cli` or from an integration test that claims to exercise real
//! transport; `scripts/check-architecture.sh` asserts that no other crate turns
//! the feature on.

mod fakes;

pub use fakes::{
    CountingRandomness, FailingRandomness, FakeClipboard, FakeClock, FakeIdSource, FakeSshConfig,
    RecordingRelay, RecordingTransport, TransportCall,
};
