//! Uploading one file so that a half-written one can never be read.
//!
//! The bytes go to a temporary name, the size is verified, and only then is
//! the file renamed into place. An agent watching the batch directory sees the
//! file appear complete or not at all.
//!
//! The temporary file is created exclusively and made private on its open
//! handle before its first byte, so there is no moment at which it exists with
//! looser permissions. Clift reads the local file itself and sends it in
//! pieces, which is also why a local path never has to be spelled in a form
//! some other program understands.

use crate::errmap::map_refusal;
use crate::probe::OpenSshTransport;
use crate::session::Failure;
use crate::wire::Attrs;
use clift_core::domain::RemotePath;
use clift_core::error::{CliftError, ErrorKind, Remedy, Stage};
use clift_core::ports::{RemoteUpload, TransportTarget};
use std::fs::File;
use std::path::Path;

/// Permissions of an uploaded attachment. The remote account's own files, and
/// nobody else's business.
pub const FILE_MODE: u32 = 0o600;

/// Bytes of randomness in the intermediate name. Enough that two uploads racing
/// into one directory cannot pick the same one.
const TEMP_BYTES: usize = 8;

impl OpenSshTransport {
    /// Uploads `source` to `destination`, atomically.
    ///
    /// # Errors
    /// Fails when the transfer fails and when the remote size does not match
    /// the local one. In both cases the destination does not exist afterwards,
    /// and the error carries no remote path: the specification forbids handing
    /// out a path to a file that is not there.
    pub fn upload_atomic(
        &self,
        target: &TransportTarget,
        source: &Path,
        destination: &RemotePath,
    ) -> Result<u64, CliftError> {
        let unreadable = |error: std::io::Error| {
            CliftError::new(
                Stage::Transfer,
                ErrorKind::Transfer,
                format!("could not read {}", source.display()),
            )
            .with_source(error)
        };
        let mut file = File::open(source).map_err(unreadable)?;
        let expected = file.metadata().map_err(unreadable)?.len();
        let temporary = temporary_path(destination)?;

        let outcome = self.runner().sftp(target, |session| {
            let attrs = match session.upload(&mut file, temporary.as_str(), FILE_MODE) {
                Ok(attrs) => attrs,
                Err(failure @ Failure::Broken { .. }) => return Err(failure),
                Err(failure) => {
                    // The reason the upload failed is what the user needs;
                    // whether the tidying up worked is not, and a leftover
                    // `.part` is what cleanup exists for.
                    let _ = session.remove(temporary.as_str());
                    return Err(failure);
                }
            };
            if let Err(wrong) = verify(target, expected, attrs) {
                let _ = session.remove(temporary.as_str());
                return Ok(Err(wrong));
            }
            match session.rename(temporary.as_str(), destination.as_str()) {
                Ok(()) => Ok(Ok(expected)),
                Err(Failure::Refused(refusal)) => {
                    let _ = session.remove(temporary.as_str());
                    Ok(Err(map_refusal(
                        target,
                        Stage::Transfer,
                        "could not put the uploaded file in place",
                        refusal.code,
                        &refusal.message,
                    )))
                }
                Err(other) => Err(other),
            }
        });

        match outcome {
            Ok(verdict) => verdict,
            Err(failure) => Err(self.runner().session_error(
                target,
                Stage::Transfer,
                "could not upload the attachment",
                failure,
            )),
        }
    }
}

/// Refuses a file whose remote size or permissions are not what was sent.
///
/// It takes no path, and that is deliberate rather than incidental: the
/// specification forbids naming a remote file that is about to be deleted,
/// and a function that never receives the path cannot leak it into the
/// message.
fn verify(target: &TransportTarget, expected: u64, attrs: Attrs) -> Result<(), CliftError> {
    let Some(reported) = attrs.size else {
        return Err(CliftError::new(
            Stage::Transfer,
            ErrorKind::Transfer,
            format!(
                "{} did not report the size of the uploaded file",
                target.ssh_host()
            ),
        ));
    };
    if reported != expected {
        return Err(CliftError::new(
            Stage::Transfer,
            ErrorKind::Transfer,
            format!("the upload was truncated: {expected} bytes were sent, {reported} arrived"),
        )
        .with_remedy(Remedy::new(
            "Check the connection and the remote free space, then send again:",
            format!("ssh {} df -h ~", target.ssh_host()),
        )));
    }
    match attrs.permissions.map(|bits| bits & 0o777) {
        Some(FILE_MODE) => Ok(()),
        other => Err(CliftError::new(
            Stage::Transfer,
            ErrorKind::RemoteDirectory,
            format!(
                "{} left the uploaded file with mode {}, not {FILE_MODE:04o}",
                target.ssh_host(),
                other.map_or_else(|| "unknown".to_string(), |bits| format!("{bits:04o}"))
            ),
        )),
    }
}

/// The intermediate name, alongside the destination.
///
/// Hidden, random and suffixed `.part`: hidden and suffixed so that a stray one
/// is recognisable as Clift's, random so that two uploads racing into the same
/// directory cannot choose the same name.
fn temporary_path(destination: &RemotePath) -> Result<RemotePath, CliftError> {
    let mut bytes = [0u8; TEMP_BYTES];
    getrandom::fill(&mut bytes).map_err(|error| {
        CliftError::new(
            Stage::Transfer,
            ErrorKind::Internal,
            "the operating system random source is unavailable",
        )
        .with_source(error)
    })?;

    let mut name = String::from(".");
    for byte in bytes {
        name.push_str(&format!("{byte:02x}"));
    }
    name.push_str(".part");

    let text = destination.as_str();
    let cut = text.rfind('/').unwrap_or(0);
    RemotePath::new(format!("{}/{name}", &text[..cut]))
        .map_err(|error| error.into_clift(Stage::Transfer, ErrorKind::Internal))
}

impl RemoteUpload for OpenSshTransport {
    fn upload_atomic(
        &self,
        target: &TransportTarget,
        source: &Path,
        destination: &RemotePath,
    ) -> Result<u64, CliftError> {
        OpenSshTransport::upload_atomic(self, target, source, destination)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn destination() -> RemotePath {
        RemotePath::new("/home/dev/.cache/clift/inbox/2026-08-30/abc/shot.png")
            .unwrap_or_else(|error| panic!("bad test path: {error}"))
    }

    fn arrived(size: Option<u64>, bits: u32) -> Attrs {
        Attrs {
            size,
            permissions: Some(bits),
            mtime: None,
        }
    }

    #[test]
    fn the_intermediate_file_sits_beside_the_destination_and_is_recognisable() {
        let temporary = temporary_path(&destination()).unwrap_or_else(|e| panic!("{e}"));
        let text = temporary.as_str();
        assert!(
            text.starts_with("/home/dev/.cache/clift/inbox/2026-08-30/abc/."),
            "{text}"
        );
        assert!(text.ends_with(".part"), "{text}");
        // Same directory: a rename across filesystems would not be atomic.
        assert_eq!(
            text.rsplit_once('/').map(|(dir, _)| dir),
            destination().as_str().rsplit_once('/').map(|(dir, _)| dir)
        );
    }

    #[test]
    fn two_intermediate_names_are_never_the_same() {
        let mut seen = std::collections::HashSet::new();
        for _ in 0..1_000 {
            let path = temporary_path(&destination()).unwrap_or_else(|e| panic!("{e}"));
            assert!(
                seen.insert(path.as_str().to_string()),
                "an intermediate name repeated"
            );
        }
    }

    #[test]
    fn a_size_mismatch_is_a_transfer_failure_that_names_no_remote_file() {
        let target = TransportTarget::new("core");
        assert!(verify(&target, 182_734, arrived(Some(182_734), 0o100_600)).is_ok());
        assert!(
            verify(&target, 0, arrived(Some(0), 0o100_600)).is_ok(),
            "zero bytes is a size like any other"
        );

        let error = verify(&target, 182_734, arrived(Some(9_216), 0o100_600))
            .expect_err("a short arrival must not be accepted");
        assert_eq!(error.exit_code().as_u8(), 23);
        assert_eq!(error.stage(), Stage::Transfer);

        let rendered = format!(
            "{error} {}",
            error
                .remedy()
                .map(|remedy| format!("{} {}", remedy.description(), remedy.command()))
                .unwrap_or_default()
        );
        for forbidden in [".part", "/inbox/", "/home/"] {
            assert!(
                !rendered.contains(forbidden),
                "a truncated upload must not hand out a path: {rendered}"
            );
        }
    }

    #[test]
    fn an_upload_without_a_size_or_with_loose_permissions_is_refused() {
        let target = TransportTarget::new("core");
        assert!(verify(&target, 10, arrived(None, 0o100_600)).is_err());
        let loose = verify(&target, 10, arrived(Some(10), 0o100_644)).unwrap_err();
        assert_eq!(loose.exit_code().as_u8(), 25);
        assert!(loose.message().contains("0644"), "{loose}");
    }
}
