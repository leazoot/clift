//! Uploading one file so that a half-written one can never be read.
//!
//! The bytes go to a temporary name, the size is verified, and only
//! then is the file renamed into place. An agent watching the batch directory
//! sees the file appear complete or not at all.
//!
//! Two round trips are the floor here, and the reason is worth stating: the
//! size has to be checked *before* the rename, and an SFTP batch cannot branch
//! on a result. Doing it in one batch would mean renaming first and checking
//! afterwards, which is exactly the half-written file this exists to prevent.

use crate::errmap::map_failure;
use crate::probe::OpenSshTransport;
use crate::proc::SftpBatch;
use clift_core::domain::RemotePath;
use clift_core::error::{CliftError, ErrorKind, Remedy, Stage};
use clift_core::ports::{RemoteUpload, TransportTarget};
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
        let expected = std::fs::metadata(source)
            .map_err(|error| {
                CliftError::new(
                    Stage::Transfer,
                    ErrorKind::Transfer,
                    format!("could not read {}", source.display()),
                )
                .with_source(error)
            })?
            .len();

        let Some(local) = source.to_str() else {
            return Err(CliftError::new(
                Stage::Transfer,
                ErrorKind::Transfer,
                format!(
                    "{} is not valid UTF-8 and cannot be named to sftp",
                    source.display()
                ),
            ));
        };

        let temporary = temporary_path(destination)?;
        let uploaded = self.upload_to_temporary(target, local, &temporary, expected);
        match uploaded {
            Ok(()) => {}
            Err(error) => {
                self.discard(target, &temporary);
                return Err(error);
            }
        }

        let mut batch = SftpBatch::new();
        batch.push("rename", &[temporary.as_str(), destination.as_str()])?;
        let outcome = self.runner().run_sftp(target, &batch)?;
        if !outcome.succeeded() {
            self.discard(target, &temporary);
            return Err(map_failure(
                target,
                Stage::Transfer,
                "could not put the uploaded file in place",
                &outcome.stderr,
            ));
        }
        Ok(expected)
    }

    /// Sends the bytes, tightens the permissions and reads the size back, in
    /// one round trip.
    fn upload_to_temporary(
        &self,
        target: &TransportTarget,
        local: &str,
        temporary: &RemotePath,
        expected: u64,
    ) -> Result<(), CliftError> {
        let mut batch = SftpBatch::new();
        batch.push("put", &[local, temporary.as_str()])?;
        // Before anything else can see it: `put` leaves the local file's mode,
        // which is whatever the screenshot tool chose.
        batch.push("chmod", &[&format!("{FILE_MODE:o}"), temporary.as_str()])?;
        batch.push("ls", &["-l", temporary.as_str()])?;

        let outcome = self.runner().run_sftp(target, &batch)?;
        if !outcome.succeeded() {
            return Err(map_failure(
                target,
                Stage::Transfer,
                "could not upload the attachment",
                &outcome.stderr,
            ));
        }

        let reported = listed_size(&outcome.stdout, temporary).ok_or_else(|| {
            CliftError::new(
                Stage::Transfer,
                ErrorKind::Transfer,
                format!(
                    "{} did not report the size of the uploaded file",
                    target.ssh_host()
                ),
            )
        })?;

        verify_size(target, expected, reported)
    }

    /// Removes an intermediate file, ignoring whatever happens.
    ///
    /// A cleanup failure must not replace the error that caused it: the user
    /// needs to know why the upload failed, not why the tidying up did.
    fn discard(&self, target: &TransportTarget, temporary: &RemotePath) {
        let mut batch = SftpBatch::new();
        if batch.push("rm", &[temporary.as_str()]).is_ok() {
            let _ = self.runner().run_sftp(target, &batch);
        }
    }
}

/// Refuses a file whose remote size does not match the local one.
///
/// It takes no path, and that is deliberate rather than incidental: the specification
/// forbids naming a remote file that is about to be deleted, and a
/// function that never receives the path cannot leak it into the message.
fn verify_size(target: &TransportTarget, expected: u64, reported: u64) -> Result<(), CliftError> {
    if reported == expected {
        return Ok(());
    }
    Err(CliftError::new(
        Stage::Transfer,
        ErrorKind::Transfer,
        format!("the upload was truncated: {expected} bytes were sent, {reported} arrived"),
    )
    .with_remedy(Remedy::new(
        "Check the connection and the remote free space, then send again:",
        format!("ssh {} df -h ~", target.ssh_host()),
    )))
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

/// The size `sftp` reported for a path in its `ls -l` output.
fn listed_size(stdout: &str, path: &RemotePath) -> Option<u64> {
    let wanted = path.as_str().rsplit('/').next()?;
    stdout
        .lines()
        .filter(|line| !line.trim_start().starts_with("sftp>"))
        .filter(|line| line.trim_end().ends_with(wanted))
        .find_map(|line| line.split_whitespace().nth(4)?.parse::<u64>().ok())
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
    fn the_size_is_read_from_the_line_for_that_file() {
        let path = RemotePath::new("/home/dev/batch/.0011223344556677.part")
            .unwrap_or_else(|error| panic!("bad test path: {error}"));
        let stdout = concat!(
            "sftp> put \"/tmp/shot.png\" \"/home/dev/batch/.0011223344556677.part\"\n",
            "sftp> ls -l \"/home/dev/batch/.0011223344556677.part\"\n",
            "-rw-------    ? dev      dev        182734 Aug 30 12:34 ",
            "/home/dev/batch/.0011223344556677.part\n"
        );
        assert_eq!(listed_size(stdout, &path), Some(182_734));
    }

    #[test]
    fn a_size_mismatch_is_a_transfer_failure_that_names_no_remote_file() {
        let target = TransportTarget::new("core");
        assert!(verify_size(&target, 182_734, 182_734).is_ok());

        let error =
            verify_size(&target, 182_734, 9_216).expect_err("a short arrival must not be accepted");
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
    fn a_zero_byte_file_reports_zero_rather_than_nothing() {
        let path = RemotePath::new("/home/dev/batch/.aabbccddeeff0011.part")
            .unwrap_or_else(|error| panic!("bad test path: {error}"));
        let stdout = "-rw-------    ? dev      dev             0 Aug 30 12:34 \
                      /home/dev/batch/.aabbccddeeff0011.part\n";
        assert_eq!(listed_size(stdout, &path), Some(0));
    }
}
