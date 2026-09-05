//! Batch limits, merged from defaults and configuration.

use super::{DomainError, LocalAttachment};

/// Ceilings applied to one upload batch.
///
/// Checked before a single byte is uploaded, so that going over the limit costs
/// the user nothing and leaves no `.part` file behind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    max_file_size: u64,
    max_batch_size: u64,
    max_files: u32,
}

/// The specification defaults.
pub const DEFAULT_MAX_FILE_SIZE: u64 = 50 * 1024 * 1024;
pub const DEFAULT_MAX_BATCH_SIZE: u64 = 100 * 1024 * 1024;
pub const DEFAULT_MAX_FILES: u32 = 20;

impl Limits {
    /// # Errors
    /// Rejects zero values and a per-file ceiling above the per-batch ceiling,
    /// which would be unreachable and so silently misleading.
    pub fn new(
        max_file_size: u64,
        max_batch_size: u64,
        max_files: u32,
    ) -> Result<Self, DomainError> {
        let subject = "limits";
        if max_file_size == 0 {
            return Err(DomainError::new(
                subject,
                "max_file_size must be greater than 0",
            ));
        }
        if max_batch_size == 0 {
            return Err(DomainError::new(
                subject,
                "max_batch_size must be greater than 0",
            ));
        }
        if max_files == 0 {
            return Err(DomainError::new(
                subject,
                "max_files must be greater than 0",
            ));
        }
        if max_file_size > max_batch_size {
            return Err(DomainError::new(
                subject,
                format!(
                    "max_file_size ({max_file_size}) must not exceed max_batch_size ({max_batch_size})"
                ),
            ));
        }
        Ok(Self {
            max_file_size,
            max_batch_size,
            max_files,
        })
    }

    #[must_use]
    pub const fn max_file_size(&self) -> u64 {
        self.max_file_size
    }

    #[must_use]
    pub const fn max_batch_size(&self) -> u64 {
        self.max_batch_size
    }

    #[must_use]
    pub const fn max_files(&self) -> u32 {
        self.max_files
    }

    /// Refuses a batch that is over any of the three ceilings.
    ///
    /// This is the whole of the limit check, and where it is called from matters as much
    /// as what it does: the check has to run before the first byte leaves the
    /// machine, so that an oversized batch costs nothing and leaves no `.part`
    /// file behind. The per-file check comes first because "which file is too
    /// big" is more useful than "the batch is too big" when one attachment is
    /// the problem.
    ///
    /// The configuration key that would raise the ceiling a batch crossed.
    ///
    /// Returned alongside the refusal so the caller can offer one command that
    /// actually addresses this failure rather than a generic "check your
    /// configuration".
    #[must_use]
    pub fn key_for(&self, attachments: &[LocalAttachment]) -> &'static str {
        if attachments.len() > self.max_files as usize {
            return "defaults.max_files";
        }
        if attachments
            .iter()
            .any(|attachment| attachment.size() > self.max_file_size)
        {
            return "defaults.max_file_size";
        }
        "defaults.max_batch_size"
    }

    /// # Errors
    /// Names the ceiling that was crossed, the offending value and the limit in
    /// force, so the message is actionable without a second command.
    pub fn check(&self, attachments: &[LocalAttachment]) -> Result<(), DomainError> {
        let subject = "batch";

        let count = attachments.len();
        if count > self.max_files as usize {
            return Err(DomainError::new(
                subject,
                format!(
                    "{count} files is more than the limit of {} per batch",
                    self.max_files
                ),
            ));
        }

        for attachment in attachments {
            if attachment.size() > self.max_file_size {
                return Err(DomainError::new(
                    subject,
                    format!(
                        "{} is {}, above the {} limit for one file",
                        attachment.name(),
                        format_size(attachment.size()),
                        format_size(self.max_file_size)
                    ),
                ));
            }
        }

        // Saturating rather than wrapping: a total that overflowed would come
        // out small and pass a check it should fail.
        let total = attachments.iter().fold(0u64, |sum, attachment| {
            sum.saturating_add(attachment.size())
        });
        if total > self.max_batch_size {
            return Err(DomainError::new(
                subject,
                format!(
                    "{count} files total {}, above the {} limit for one batch",
                    format_size(total),
                    format_size(self.max_batch_size)
                ),
            ));
        }

        Ok(())
    }
}

/// Renders a byte count the way `config.toml` writes one.
///
/// Exact multiples get their unit; anything else stays in bytes rather than
/// being rounded. "51 MiB is above the 50 MiB limit" reads like a rounding bug
/// even when it is true, so a value that does not divide evenly is shown whole.
fn format_size(bytes: u64) -> String {
    const UNITS: [(u64, &str); 3] = [
        (1024 * 1024 * 1024, "GiB"),
        (1024 * 1024, "MiB"),
        (1024, "KiB"),
    ];
    for (scale, name) in UNITS {
        if bytes >= scale && bytes.is_multiple_of(scale) {
            return format!("{} {name}", bytes / scale);
        }
    }
    format!("{bytes} bytes")
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_file_size: DEFAULT_MAX_FILE_SIZE,
            max_batch_size: DEFAULT_MAX_BATCH_SIZE,
            max_files: DEFAULT_MAX_FILES,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_fr_033() {
        let limits = Limits::default();
        assert_eq!(limits.max_file_size(), 50 * 1024 * 1024);
        assert_eq!(limits.max_batch_size(), 100 * 1024 * 1024);
        assert_eq!(limits.max_files(), 20);
    }

    #[test]
    fn defaults_satisfy_their_own_invariants() {
        let d = Limits::default();
        assert!(Limits::new(d.max_file_size(), d.max_batch_size(), d.max_files()).is_ok());
    }

    #[test]
    fn rejects_zero_values() {
        assert!(Limits::new(0, 100, 20).is_err());
        assert!(Limits::new(50, 0, 20).is_err());
        assert!(Limits::new(50, 100, 0).is_err());
    }

    fn attachment(name: &str, size: u64) -> LocalAttachment {
        LocalAttachment::new(
            std::path::PathBuf::from(format!("/tmp/{name}")),
            crate::domain::SafeFileName::sanitize(name),
            size,
            crate::domain::FileKind::Regular,
        )
        .unwrap_or_else(|error| panic!("bad test attachment: {error}"))
    }

    fn batch_of(count: usize, each: u64) -> Vec<LocalAttachment> {
        (0..count)
            .map(|index| attachment(&format!("shot-{index}.png"), each))
            .collect()
    }

    /// The boundary is the requirement: 20 files pass, 21 do not.
    #[test]
    fn the_file_count_boundary_is_exactly_where_fr_033_puts_it() {
        let limits = Limits::default();
        assert!(limits.check(&batch_of(20, 1)).is_ok());

        let error = limits
            .check(&batch_of(21, 1))
            .expect_err("21 files is one too many");
        assert!(error.reason().contains("21 files"), "{error}");
        assert!(error.reason().contains("20"), "{error}");
    }

    /// Exactly 50 MiB is allowed; one byte more is not.
    #[test]
    fn the_per_file_boundary_is_exact_to_the_byte() {
        let limits = Limits::default();
        let exactly = DEFAULT_MAX_FILE_SIZE;
        assert!(limits.check(&[attachment("shot.png", exactly)]).is_ok());

        let error = limits
            .check(&[attachment("shot.png", exactly + 1)])
            .expect_err("one byte over the per-file limit");
        assert!(error.reason().contains("shot.png"), "{error}");
        assert!(error.reason().contains("50 MiB"), "{error}");
        assert!(
            error.reason().contains("52428801 bytes"),
            "the offending size must be exact, not rounded to 50 MiB: {error}"
        );
    }

    /// Exactly 100 MiB across several files is allowed; one byte more is not.
    /// Every file stays under the per-file ceiling, so the batch total is
    /// genuinely what is being tested.
    #[test]
    fn the_batch_total_boundary_is_exact_to_the_byte() {
        let limits = Limits::default();
        let quarter = DEFAULT_MAX_BATCH_SIZE / 4;
        assert!(limits.check(&batch_of(4, quarter)).is_ok());

        let mut over = batch_of(4, quarter);
        over.push(attachment("one-more.txt", 1));
        let error = limits
            .check(&over)
            .expect_err("one byte over the batch limit");
        assert!(error.reason().contains("100 MiB"), "{error}");
        assert!(error.reason().contains("5 files"), "{error}");
    }

    /// Which file is too big beats "the batch is too big" when one attachment
    /// is the whole problem.
    #[test]
    fn an_oversized_file_is_named_rather_than_blamed_on_the_batch() {
        let limits = Limits::default();
        let error = limits
            .check(&[attachment("huge.mov", DEFAULT_MAX_BATCH_SIZE + 1)])
            .expect_err("above both ceilings");
        assert!(error.reason().contains("huge.mov"), "{error}");
        assert!(
            error.reason().contains("for one file"),
            "the per-file ceiling is the one to report: {error}"
        );
    }

    #[test]
    fn a_configured_limit_moves_the_boundary_with_it() {
        let limits = Limits::new(1024, 2048, 3).unwrap_or_else(|error| panic!("{error}"));
        assert!(limits.check(&batch_of(2, 1024)).is_ok());

        assert!(
            limits.check(&batch_of(4, 1)).is_err(),
            "four files under a limit of three"
        );
        assert!(
            limits.check(&[attachment("a.bin", 1025)]).is_err(),
            "one byte over the configured per-file limit"
        );
        let error = limits
            .check(&batch_of(3, 1024))
            .expect_err("3 KiB is over the configured 2 KiB batch limit");
        assert!(error.reason().contains("2 KiB"), "{error}");
    }

    /// A total that wrapped would come out small and pass a check it must fail.
    #[test]
    fn a_total_that_would_overflow_still_fails() {
        let limits = Limits::default();
        assert!(
            limits
                .check(&[attachment("a.bin", u64::MAX), attachment("b.bin", u64::MAX)])
                .is_err()
        );
    }

    #[test]
    fn the_key_named_is_the_one_that_would_admit_the_batch() {
        let limits = Limits::default();
        assert_eq!(limits.key_for(&batch_of(21, 1)), "defaults.max_files");
        assert_eq!(
            limits.key_for(&[attachment("huge.mov", DEFAULT_MAX_FILE_SIZE + 1)]),
            "defaults.max_file_size"
        );
        let mut over = batch_of(4, DEFAULT_MAX_BATCH_SIZE / 4);
        over.push(attachment("one-more.txt", 1));
        assert_eq!(limits.key_for(&over), "defaults.max_batch_size");
    }

    #[test]
    fn an_empty_batch_is_within_every_limit() {
        assert!(Limits::default().check(&[]).is_ok());
    }

    #[test]
    fn sizes_are_rendered_in_the_unit_they_divide_into() {
        assert_eq!(format_size(50 * 1024 * 1024), "50 MiB");
        assert_eq!(format_size(2 * 1024 * 1024 * 1024), "2 GiB");
        assert_eq!(format_size(512 * 1024), "512 KiB");
        assert_eq!(format_size(0), "0 bytes");
        assert_eq!(format_size(50 * 1024 * 1024 + 1), "52428801 bytes");
    }

    #[test]
    fn rejects_unreachable_per_file_ceiling() {
        assert!(Limits::new(101, 100, 20).is_err());
        assert!(Limits::new(100, 100, 20).is_ok());
    }
}
