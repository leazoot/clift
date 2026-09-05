//! Proving that a host can actually take a file.
//!
//! Reachability, authentication and an SFTP subsystem all say a host *should*
//! work. This says it does: a small file is uploaded, inspected, removed, and
//! its absence confirmed. The failures it catches -- a full home directory, a
//! read-only mount, a server that refuses `rename`, a directory that cannot be
//! written to -- all look perfectly healthy until something is written.
//!
//! Shared by `setup`, which runs it once before recording a target, and by
//! `doctor`, which runs it to answer "would a send work right now".

use crate::domain::{RemotePath, SafeFileName};
use crate::error::{CliftError, ErrorKind, Remedy, Stage};
use crate::ports::{RemoteFs, RemoteUpload, TransportTarget};
use crate::runtime::ScratchFile;

/// The file name the check uses. Recognisable as Clift's, and short enough to
/// survive any filesystem.
pub const SELF_CHECK_NAME: &str = "clift-selfcheck";

const SELF_CHECK_BODY: &[u8] = b"clift self check\n";

/// Permissions an uploaded attachment must have. Duplicated from the transport
/// on purpose: this is the rule, the transport is one implementation of it.
const REQUIRED_MODE: u32 = 0o600;

/// Uploads a small file into `directory`, checks it, and removes it again.
///
/// # Errors
/// Fails when the upload does not arrive intact, when the file does not have
/// the permissions Clift set, and when it cannot be removed again. The removal
/// is not best-effort, unlike cleanup after a send: a host that cannot delete
/// is a host whose inbox grows without limit, and the user should learn that
/// now rather than in a month.
pub fn verify_round_trip(
    remote: &dyn RemoteFs,
    upload: &dyn RemoteUpload,
    target: &TransportTarget,
    directory: &RemotePath,
) -> Result<(), CliftError> {
    let scratch = ScratchFile::create(SELF_CHECK_NAME, "txt", SELF_CHECK_BODY)?;
    let name = SafeFileName::new(SELF_CHECK_NAME)
        .map_err(|error| error.into_clift(Stage::Staging, ErrorKind::Internal))?;
    let destination = directory.join(&name);

    let uploaded = upload.upload_atomic(target, scratch.path(), &destination)?;
    if uploaded != SELF_CHECK_BODY.len() as u64 {
        return Err(failed(
            target,
            "the file that arrived was not the size of the file that was sent",
        ));
    }

    match remote.stat(target, &destination)? {
        Some(entry) if entry.mode.is_some_and(|mode| mode & 0o777 != REQUIRED_MODE) => {
            return Err(failed(
                target,
                "the uploaded file did not keep the permissions Clift set",
            ));
        }
        Some(_) => {}
        None => {
            return Err(failed(target, "the uploaded file was not there afterwards"));
        }
    }

    remote.remove(target, &destination)?;
    if remote.stat(target, &destination)?.is_some() {
        return Err(failed(target, "the test file could not be removed again"));
    }
    Ok(())
}

fn failed(target: &TransportTarget, detail: &str) -> CliftError {
    let host = target.ssh_host();
    CliftError::new(
        Stage::Transfer,
        ErrorKind::Transfer,
        format!("Upload and cleanup failed on {host}: {detail}"),
    )
    .with_remedy(Remedy::new(
        "Check the connection by hand:",
        format!("ssh {host}"),
    ))
}
