//! Connection reuse: one SSH handshake instead of nine.
//!
//! Clift runs the system `ssh` and `sftp` clients, one process per operation,
//! and every one of those processes opens its own connection. A single send is
//! nine or ten of them. On a host three time zones away, where a handshake is
//! measured in seconds rather than milliseconds, that arithmetic is the whole
//! of Clift's latency: the bytes are not the problem, the connections are.
//!
//! OpenSSH already solves this. A *master* connection stays open and later
//! clients ride over it, which is what `ControlMaster`, `ControlPath` and
//! `ControlPersist` are for. Clift asks for it rather than implementing it,
//! for the same reason it does not implement SSH: the user's authentication,
//! host key checking, agent, hardware key and jump hosts all keep working
//! because they are still OpenSSH's, not Clift's.
//!
//! Three rules shape what is here, all from the specification:
//!
//! 1. **The socket lives in Clift's own private directory** (`0700`), not in a
//!    shared one. A control socket is an open, authenticated connection to the
//!    remote host: anyone who can write next to it, or connect to it, has the
//!    session.
//! 2. **A user who already multiplexes is left alone.** Command line options
//!    beat the configuration file in OpenSSH, so honouring an existing
//!    `ControlMaster` means not passing the options at all. The decision is
//!    made in [`clift_core::context::multiplexes_already`] from what `ssh -G`
//!    reports; see [`crate::proc::SshRunner`].
//! 3. **Failure falls back to an ordinary connection.** Every way this can go
//!    wrong -- no private directory, a socket path the kernel cannot hold, a
//!    stale socket, a master that died -- ends with a normal connection rather
//!    than an error. Reuse is faster, never required.
//!
//! What it is not is a daemon. The master is a plain `ssh` process, visible in
//! `ps`, holding no CPU, that exits by itself once the configured idle time
//! passes.

use clift_core::error::{CliftError, ErrorKind, Remedy, Stage};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// The token OpenSSH expands into a hash of the user, host, port and jump
/// host. One socket per distinct destination falls out of it, and no host name
/// is written into a filename.
const DESTINATION_HASH: &str = "%C";

/// How many characters [`DESTINATION_HASH`] becomes: a SHA-1 digest in hex.
///
/// Measured from real output rather than assumed; the fixture that shows it is
/// `tests/fixtures/ssh-config/multiplexed.txt`.
const DESTINATION_HASH_WIDTH: usize = 40;

/// What OpenSSH appends to the socket path while it is being created.
///
/// A master binds `<path>.<sixteen random characters>` and renames it into
/// place, so the name the kernel actually sees is seventeen characters longer
/// than the one Clift asked for. Budgeting for the shorter one produces a path
/// that looks fine and then fails inside `ssh` with
/// `unix_listener: path "..." too long for Unix domain socket` -- which is how
/// this constant was found, by `connection_reuse.rs` against a real server,
/// rather than by reading the source of OpenSSH.
const TEMPORARY_SUFFIX_WIDTH: usize = 1 + 16;

/// The longest a unix socket path may be.
///
/// `sun_path` is 104 bytes on macOS and 108 on Linux, so the smaller of the
/// two is the portable answer. OpenSSH refuses a longer `ControlPath` outright,
/// which would turn every operation into an error -- hence the check here, and
/// the fall back to no reuse rather than a failure.
const SOCKET_PATH_LIMIT: usize = 104;

/// Where a reused connection lives and how long it is kept.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reuse {
    control_path: PathBuf,
    persist: Duration,
}

impl Reuse {
    /// Prepares reuse inside Clift's private run directory.
    ///
    /// # Errors
    /// Fails when the private directory cannot be prepared, and when the
    /// socket path it produces would be too long for the kernel. Both are
    /// reasons to carry on without reuse, not reasons to stop: the caller is
    /// expected to say so and continue.
    pub fn in_private_dir(persist: Duration) -> Result<Self, CliftError> {
        unsupported_platform()?;
        Self::in_directory(&clift_core::runtime::private_run_dir()?, persist)
    }

    /// The same, in a directory the caller names. For tests and for a machine
    /// whose private directory is somewhere unusual.
    ///
    /// # Errors
    /// Fails when the resulting socket path would exceed [`SOCKET_PATH_LIMIT`].
    pub fn in_directory(directory: &Path, persist: Duration) -> Result<Self, CliftError> {
        let control_path = directory.join(DESTINATION_HASH);
        let expanded = control_path.as_os_str().len() - DESTINATION_HASH.len()
            + DESTINATION_HASH_WIDTH
            + TEMPORARY_SUFFIX_WIDTH;
        if expanded > SOCKET_PATH_LIMIT {
            return Err(CliftError::new(
                Stage::Connect,
                ErrorKind::Config,
                format!(
                    "the control socket path under {} would be {expanded} characters, past the \
                     {SOCKET_PATH_LIMIT} a unix socket can hold",
                    directory.display()
                ),
            )
            .with_remedy(Remedy::new(
                "Put Clift's private directory somewhere shorter, or turn reuse off:",
                "clift config set connection.reuse false",
            )));
        }
        Ok(Self {
            control_path,
            persist,
        })
    }

    /// The socket path, with the token still in it: `ssh` expands it, not us.
    #[must_use]
    pub fn control_path(&self) -> &Path {
        &self.control_path
    }

    /// The options `ssh` and `sftp` are given, and the only options Clift ever
    /// generates.
    ///
    /// `auto` rather than `yes`: a client that finds no socket becomes the
    /// master, and one that finds a live socket uses it. `yes` would mean
    /// "always be the master", which turns a second concurrent Clift into an
    /// error instead of a second passenger.
    ///
    /// The persist time is rendered in seconds because that is unambiguous;
    /// OpenSSH prints it back that way too.
    #[must_use]
    pub fn options(&self) -> Vec<OsString> {
        let mut path = OsString::from("ControlPath=");
        path.push(&self.control_path);
        vec![
            OsString::from("-o"),
            OsString::from("ControlMaster=auto"),
            OsString::from("-o"),
            path,
            OsString::from("-o"),
            OsString::from(format!("ControlPersist={}", self.persist.as_secs())),
        ]
    }
}

/// Refuses on a platform whose OpenSSH client cannot do this.
///
/// The specification ends with "Windows must not fake an equivalent capability", and the
/// Windows port of OpenSSH does not implement `ControlMaster` at all: passing
/// the options there produces a client that either complains or quietly
/// ignores them, and in both cases every operation still opens its own
/// connection. Saying so is the honest form of "not supported"; the caller
/// carries on without reuse.
#[cfg(windows)]
fn unsupported_platform() -> Result<(), CliftError> {
    Err(CliftError::new(
        Stage::Connect,
        ErrorKind::Config,
        "the Windows OpenSSH client does not implement ControlMaster, so connections cannot be \
         reused here",
    ))
}

#[cfg(not(windows))]
const fn unsupported_platform() -> Result<(), CliftError> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reuse(directory: &str) -> Result<Reuse, CliftError> {
        Reuse::in_directory(Path::new(directory), Duration::from_secs(600))
    }

    #[test]
    fn the_three_options_are_exactly_what_openssh_needs_and_nothing_more() {
        let options = reuse("/home/x/.cache/clift/run").unwrap().options();
        let rendered: Vec<String> = options
            .iter()
            .map(|option| option.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            rendered,
            vec![
                "-o",
                "ControlMaster=auto",
                "-o",
                "ControlPath=/home/x/.cache/clift/run/%C",
                "-o",
                "ControlPersist=600",
            ]
        );
    }

    /// The token is passed through rather than expanded here. Expanding it
    /// would mean reimplementing OpenSSH's hash of user, host, port and jump
    /// host -- and a Clift that computed it differently would open a second
    /// master beside the one already running.
    #[test]
    fn the_destination_hash_is_left_for_ssh_to_expand() {
        let reuse = reuse("/x").unwrap();
        assert!(reuse.control_path().to_string_lossy().ends_with("/%C"));
    }

    /// A path the kernel cannot hold is a fall back to an ordinary connection,
    /// which is why this is an error the caller can look at rather than a
    /// silent shortening.
    #[test]
    fn a_socket_path_too_long_for_the_kernel_is_refused_with_a_way_out() {
        let deep = format!("/home/{}/run", "d".repeat(90));
        let error = reuse(&deep).unwrap_err();
        assert!(error.message().contains("unix socket"), "{error}");
        assert_eq!(
            error.remedy().map(clift_core::error::Remedy::command),
            Some("clift config set connection.reuse false")
        );
    }

    /// The limit is on the *expanded* path, and the token is two characters
    /// standing in for forty. Measuring the unexpanded one would accept a path
    /// that then fails inside `ssh`, where the fall back cannot reach it.
    /// The limit is on the name the kernel sees: the directory, the forty
    /// characters `%C` becomes, and the seventeen more OpenSSH appends while
    /// it binds the socket. Measuring anything shorter accepts a path that
    /// then fails inside `ssh`, where the fall back cannot reach it.
    #[test]
    fn the_length_that_matters_is_the_one_the_kernel_sees() {
        // 46 characters of directory: 46 + 1 + 40 + 17 = 104, the limit exactly.
        let directory = format!("/{}", "d".repeat(45));
        assert_eq!(directory.len(), 46);
        assert!(reuse(&directory).is_ok());
        assert!(
            reuse(&format!("{directory}d")).is_err(),
            "one character further is one character too far"
        );
    }
}
