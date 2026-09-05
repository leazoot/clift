//! Asking the system `ssh` client what it would do with an alias.
//!
//! This adapter runs `ssh -G <alias>`, which resolves the user's configuration
//! and prints the result without connecting to anything. Reading the answer is
//! `clift-core`'s job; producing it is the client's, and that division is the
//! whole point (see `clift_core::context::sshconfig`).

use crate::errmap::map_failure;
use crate::probe::OpenSshTransport;
use clift_core::context::{SshHostSettings, parse_effective_config};
use clift_core::error::{CliftError, Stage};
use clift_core::ports::{SshConfigSource, TransportTarget};

impl OpenSshTransport {
    /// The effective SSH settings for an alias.
    ///
    /// # Errors
    /// Fails when `ssh` cannot be run, and when it refuses the alias -- an
    /// unknown alias is a configuration problem the user has to fix before
    /// anything else can work.
    pub fn settings_for(&self, alias: &str) -> Result<SshHostSettings, CliftError> {
        let target = TransportTarget::new(alias);
        let outcome = self.runner().run_ssh_config_dump(&target)?;
        if !outcome.succeeded() {
            return Err(map_failure(
                &target,
                Stage::Config,
                "could not read the SSH configuration for this host",
                &outcome.stderr,
            ));
        }
        parse_effective_config(alias, &outcome.stdout)
    }
}

impl SshConfigSource for OpenSshTransport {
    fn settings_for(&self, alias: &str) -> Result<SshHostSettings, CliftError> {
        OpenSshTransport::settings_for(self, alias)
    }
}
