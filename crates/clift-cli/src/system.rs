//! The ports that are simply the machine.
//!
//! A clock, a source of batch identifiers and a source of raw entropy exist as
//! ports so that "which date directory", "which batch" and "which key" can each
//! be pinned down in a test. Outside a test there is only ever one answer to
//! each, and it lives here rather than once per command.

use crate::output::Reporter;
use clift_core::config::Config;
use clift_core::domain::{BATCH_ID_BYTES, BatchId};
use clift_core::error::{CliftError, ErrorKind, Stage};
use clift_core::ports::{Clock, IdSource, Randomness};
use clift_transport::probe::OpenSshTransport;
use clift_transport::proc::SshRunner;
use clift_transport::reuse::Reuse;
use std::time::SystemTime;

/// The transport, with connection reuse resolved once.
///
/// Every command that touches a host builds it here rather than each deciding
/// for itself, for the reason the rest of this file exists: there is only one
/// right answer per machine, and a command that quietly used a different one
/// would be a command with different performance and different processes left
/// behind.
///
/// Reuse never turns a working setup into a broken one. Anything that makes it
/// unavailable -- switched off, no private directory, a socket path too long,
/// an OpenSSH client that has no such feature -- says so under `--verbose` and
/// falls back to the connection-per-operation behaviour Clift had before.
pub fn transport(config: &Config, reporter: &Reporter) -> OpenSshTransport {
    let connection = config.connection();
    if !connection.reuse() {
        reporter.verbose("connection reuse is off; each operation opens its own connection");
        return OpenSshTransport::new();
    }

    // Two separate savings, and the second is the larger one. The connection is
    // reused across processes; the SFTP session is reused across the
    // operations of this one process. Reusing the connection saves the
    // authentication, reusing the session saves the remote `sftp-server`
    // starting up again -- which on the reference host cost seconds per
    // operation, against an authentication that a master had already made free.
    //
    // Both hang off the same `reuse` setting because both are what that setting
    // says: one connection per host rather than one per operation. Turning it
    // off gets the behaviour Clift had before either existed.
    let runner = SshRunner::new().with_sessions();
    match Reuse::in_private_dir(connection.persist()) {
        Ok(reuse) => {
            reporter.verbose(&format!(
                "reusing one connection per host, kept {} seconds after the last use, \
                 and one sftp session for this run",
                connection.persist().as_secs()
            ));
            OpenSshTransport::with_runner(runner.with_reuse(reuse))
        }
        Err(error) => {
            reporter.verbose(&format!(
                "connection reuse unavailable, carrying on with one sftp session per run: {}",
                error.message()
            ));
            OpenSshTransport::with_runner(runner)
        }
    }
}

pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> SystemTime {
        SystemTime::now()
    }
}

pub struct SystemIdSource;

impl IdSource for SystemIdSource {
    /// Straight from the operating system's CSPRNG.
    ///
    /// A failure here aborts whatever asked for it. There is no fallback to a
    /// timestamp or a counter: a predictable identifier would tell another
    /// account on the remote host where to look, which is the one thing this
    /// value exists to prevent.
    fn new_batch_id(&self) -> Result<BatchId, CliftError> {
        let mut bytes = [0_u8; BATCH_ID_BYTES];
        getrandom::fill(&mut bytes).map_err(|error| {
            CliftError::new(
                Stage::Staging,
                ErrorKind::Internal,
                "the operating system random source is unavailable",
            )
            .with_source(error)
        })?;
        BatchId::from_random_bytes(bytes)
            .map_err(|error| error.into_clift(Stage::Staging, ErrorKind::Internal))
    }
}

pub struct SystemRandomness;

impl Randomness for SystemRandomness {
    /// Straight from the operating system's CSPRNG, with no fallback.
    ///
    /// The buffer this fills becomes an object key or a nonce. There is no
    /// degraded mode for either: a predictable key would make the ciphertext on
    /// the relay readable by whoever guessed it, which is the one thing
    /// Universal Mode exists to prevent.
    fn fill(&self, buffer: &mut [u8]) -> Result<(), CliftError> {
        getrandom::fill(buffer).map_err(|error| {
            CliftError::new(
                Stage::Relay,
                ErrorKind::Internal,
                "the operating system random source is unavailable",
            )
            .with_source(error)
        })
    }
}
