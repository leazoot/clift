//! File name that is safe to create inside a remote batch directory.

use super::{DomainError, reject_control_characters};
use std::fmt;

/// A file name guaranteed to stay inside the directory it is created in.
///
/// This type only *validates*. Turning an arbitrary clipboard file name into a
/// valid one is a separate concern and is not implemented yet, so
/// today every caller must hand over a name that already satisfies the rules.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SafeFileName(String);

/// Most Linux filesystems cap a single path component at 255 bytes.
pub const MAX_LEN: usize = 255;

/// Used when nothing recognisable survives sanitisation.
pub const FALLBACK: &str = "attachment";

impl SafeFileName {
    /// # Errors
    /// Rejects empty names, control characters, path separators, the `.` and
    /// `..` entries, names starting with `-` (which shells and CLIs read as an
    /// option) and names longer than 255 bytes.
    pub fn new(value: impl Into<String>) -> Result<Self, DomainError> {
        let value = value.into();
        let subject = "file name";

        if value.is_empty() {
            return Err(DomainError::new(subject, "must not be empty"));
        }
        reject_control_characters(subject, &value)?;
        if value.contains('/') || value.contains('\\') {
            return Err(DomainError::new(
                subject,
                "must not contain a path separator",
            ));
        }
        if value == "." || value == ".." {
            return Err(DomainError::new(
                subject,
                "must not be a directory traversal entry",
            ));
        }
        if value.starts_with('-') {
            return Err(DomainError::new(
                subject,
                "must not start with '-', which is read as a command line option",
            ));
        }
        if value.len() > MAX_LEN {
            return Err(DomainError::new(
                subject,
                format!("must be at most {MAX_LEN} bytes, got {}", value.len()),
            ));
        }
        Ok(Self(value))
    }

    /// Turns whatever the operating system handed over into a name that is
    /// safe to place in a batch directory.
    ///
    /// This is the counterpart to [`SafeFileName::new`]: the constructor
    /// refuses a bad name, this repairs one. Refusing is right for a name Clift
    /// generates itself; repairing is right for a name the user's screenshot
    /// tool or file manager produced, because failing a paste over a stray
    /// character would be useless to the person pasting.
    ///
    /// What survives is deliberately generous. Quotes, backticks, `$`, `;` and
    /// emoji are all left alone: they are legitimate in a file name, a real
    /// SFTP server was seen to carry them unchanged, and stripping them would
    /// mangle names for no gain. Only what is genuinely unusable goes.
    #[must_use]
    pub fn sanitize(value: &str) -> Self {
        let candidate = repair(value);
        // Unreachable by construction: `repair` applies every rule `new`
        // enforces. Falling back rather than panicking means a future rule
        // added to `new` alone degrades to a usable name instead of killing a
        // paste; `sanitising_always_produces_a_valid_name` pins the pair
        // together so the gap cannot go unnoticed.
        Self::new(candidate).unwrap_or_else(|_| Self(FALLBACK.to_string()))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The extension including its dot, if the name has one.
    ///
    /// A leading dot does not start an extension: `.bashrc` is a hidden file
    /// with no extension, not an extension named `bashrc`.
    #[must_use]
    pub fn extension(&self) -> Option<&str> {
        let index = self.0.rfind('.')?;
        if index == 0 || index + 1 == self.0.len() {
            return None;
        }
        Some(&self.0[index..])
    }
}

impl fmt::Display for SafeFileName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// The sanitisation itself, kept separate from the type so that each step is
/// visible in one place.
fn repair(value: &str) -> String {
    // A file manager hands over a path, not a name; anything before the last
    // separator is not ours to keep, and dropping it is also what defeats
    // `../../etc/passwd`.
    let last = value.rsplit(['/', '\\']).next().unwrap_or(value);

    let cleaned: String = last.chars().filter(|c| !c.is_control()).collect();
    let cleaned = cleaned.trim();
    // A leading dash is read as a command line option by anything the path is
    // later pasted into.
    let cleaned = cleaned.trim_start_matches('-').trim_start();

    if cleaned.is_empty() || cleaned == "." || cleaned == ".." {
        return FALLBACK.to_string();
    }
    truncate(cleaned)
}

/// Shortens a name to [`MAX_LEN`] bytes while keeping its extension.
///
/// The extension is what tells an agent, and the user, what the file is; a
/// truncation that drops `.png` turns a screenshot into an unidentifiable blob.
fn truncate(name: &str) -> String {
    if name.len() <= MAX_LEN {
        return name.to_string();
    }

    let extension = name
        .rfind('.')
        .filter(|index| *index > 0 && index + 1 < name.len())
        .map(|index| &name[index..])
        .unwrap_or("");

    // An extension that is itself oversized cannot be preserved; cutting the
    // whole name is the only option left.
    if extension.len() >= MAX_LEN {
        return cut_to(name, MAX_LEN).to_string();
    }

    let stem = &name[..name.len() - extension.len()];
    format!("{}{extension}", cut_to(stem, MAX_LEN - extension.len()))
}

/// The longest prefix of `text` that fits in `limit` bytes without splitting a
/// character.
fn cut_to(text: &str, limit: usize) -> &str {
    if text.len() <= limit {
        return text;
    }
    let mut end = limit;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

/// Hands out names that are unique within one batch.
///
/// Two files may legitimately arrive with the same name, and sanitisation can
/// also map two different names onto one. Neither may result in one file
/// overwriting the other, so the second gets a numeric suffix.
///
/// The suffix is a counter rather than random text. Unpredictability is already
/// supplied by the batch directory, whose name is 128 bits from the OS CSPRNG;
/// adding entropy to the leaf buys nothing and costs the reader -- and these
/// names are pasted into someone's prompt, where `shot-2.png` reads better than
/// `shot-a3f2.png`.
#[derive(Debug, Clone, Default)]
pub struct BatchNames {
    taken: Vec<String>,
}

impl BatchNames {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Sanitises `value` and returns a name no other file in this batch has.
    pub fn assign(&mut self, value: &str) -> SafeFileName {
        let base = SafeFileName::sanitize(value);
        if self.claim(base.as_str()) {
            return base;
        }

        let extension = base.extension().unwrap_or("");
        let stem = &base.as_str()[..base.as_str().len() - extension.len()];
        for counter in 2u32.. {
            let suffix = format!("-{counter}");
            let room = MAX_LEN.saturating_sub(extension.len() + suffix.len());
            let candidate = format!("{}{suffix}{extension}", cut_to(stem, room));
            if self.claim(&candidate) {
                return SafeFileName::sanitize(&candidate);
            }
        }
        // `2u32..` is exhausted only after four billion identical names, which
        // the batch limit of 20 files makes unreachable.
        SafeFileName::sanitize(FALLBACK)
    }

    /// Records a name if it is free, comparing case insensitively.
    ///
    /// Some remote filesystems fold case, and two files that differ only in
    /// case would then be one file. The same reasoning already governs the
    /// lowercase hex of `BatchId`.
    fn claim(&mut self, candidate: &str) -> bool {
        let folded = candidate.to_lowercase();
        if self.taken.contains(&folded) {
            return false;
        }
        self.taken.push(folded);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_ordinary_names() {
        for name in [
            "shot.png",
            "requirements.pdf",
            "报告 最终版.pdf",
            "archive.tar.gz",
            ".bashrc",
            "no-extension",
            "spaces in name.txt",
        ] {
            assert!(SafeFileName::new(name).is_ok(), "rejected {name:?}");
        }
    }

    #[test]
    fn rejects_traversal_and_separators() {
        for name in [
            "..",
            ".",
            "../etc/passwd",
            "a/b.png",
            "a\\b.png",
            "/etc/passwd",
        ] {
            assert!(SafeFileName::new(name).is_err(), "accepted {name:?}");
        }
    }

    #[test]
    fn rejects_control_characters_and_newlines() {
        assert!(SafeFileName::new("shot\u{0}.png").is_err());
        assert!(SafeFileName::new("two\nlines.png").is_err());
    }

    #[test]
    fn rejects_leading_dash() {
        assert!(SafeFileName::new("-rf.png").is_err());
    }

    #[test]
    fn rejects_empty_and_overlong() {
        assert!(SafeFileName::new("").is_err());
        assert!(SafeFileName::new("a".repeat(MAX_LEN)).is_ok());
        assert!(SafeFileName::new("a".repeat(MAX_LEN + 1)).is_err());
    }

    #[test]
    fn sanitising_keeps_everything_a_file_name_is_allowed_to_contain() {
        for name in [
            "报告 最终版.pdf",
            "shot 2026-08-30 at 12.34.56.png",
            "it's \"quoted\" $HOME `id`; drop.png",
            "🎉 party.gif",
        ] {
            assert_eq!(
                SafeFileName::sanitize(name).as_str(),
                name,
                "sanitising altered a name that was already fine"
            );
        }
    }

    #[test]
    fn sanitising_drops_the_directory_part_which_is_what_defeats_traversal() {
        assert_eq!(
            SafeFileName::sanitize("../../../etc/passwd").as_str(),
            "passwd"
        );
        assert_eq!(SafeFileName::sanitize("/etc/shadow").as_str(), "shadow");
        assert_eq!(SafeFileName::sanitize("a\\b\\c.png").as_str(), "c.png");
    }

    #[test]
    fn sanitising_removes_control_characters_and_a_leading_dash() {
        assert_eq!(
            SafeFileName::sanitize("two\nlines.png").as_str(),
            "twolines.png"
        );
        assert_eq!(SafeFileName::sanitize("nul\u{0}.png").as_str(), "nul.png");
        assert_eq!(SafeFileName::sanitize("-rf.png").as_str(), "rf.png");
        assert_eq!(
            SafeFileName::sanitize("  padded.png  ").as_str(),
            "padded.png"
        );
    }

    #[test]
    fn sanitising_falls_back_when_nothing_recognisable_survives() {
        for name in ["", "..", ".", "---", "   ", "../..", "\u{1}\u{2}"] {
            assert_eq!(
                SafeFileName::sanitize(name).as_str(),
                FALLBACK,
                "{name:?} should have fallen back"
            );
        }
    }

    #[test]
    fn truncation_keeps_the_extension_because_it_says_what_the_file_is() {
        let long = format!("{}.png", "写".repeat(200));
        let name = SafeFileName::sanitize(&long);
        assert!(name.as_str().len() <= MAX_LEN, "{}", name.as_str().len());
        assert_eq!(name.extension(), Some(".png"));
        assert!(
            name.as_str().starts_with('写'),
            "the recognisable part must survive: {name}"
        );
    }

    #[test]
    fn truncation_never_splits_a_character() {
        for length in 80..=90 {
            let name = SafeFileName::sanitize(&format!("{}.png", "中".repeat(length)));
            assert!(name.as_str().len() <= MAX_LEN);
            // Reconstructing it proves the bytes are still valid UTF-8 text.
            assert_eq!(name.as_str(), name.to_string());
        }
    }

    #[test]
    fn sanitising_always_produces_a_valid_name() {
        let overlong = "a".repeat(MAX_LEN * 2);
        let inputs = [
            "",
            "..",
            "../../etc/passwd",
            "-rf",
            "\u{0}\u{1}\n\r\t",
            overlong.as_str(),
            "🎉",
            ".",
            "./",
            "/",
            "\\",
        ];
        for input in inputs {
            let name = SafeFileName::sanitize(input);
            assert!(
                SafeFileName::new(name.as_str()).is_ok(),
                "sanitize produced a name new() rejects: {name:?} from {input:?}"
            );
        }
    }

    #[test]
    fn a_batch_never_lets_one_file_overwrite_another() {
        let mut names = BatchNames::new();
        assert_eq!(names.assign("shot.png").as_str(), "shot.png");
        assert_eq!(names.assign("shot.png").as_str(), "shot-2.png");
        assert_eq!(names.assign("shot.png").as_str(), "shot-3.png");
        // Different directories, same leaf: still two distinct files.
        assert_eq!(names.assign("/tmp/a/shot.png").as_str(), "shot-4.png");
    }

    #[test]
    fn a_batch_treats_names_that_differ_only_in_case_as_the_same() {
        let mut names = BatchNames::new();
        assert_eq!(names.assign("Shot.PNG").as_str(), "Shot.PNG");
        assert_eq!(
            names.assign("shot.png").as_str(),
            "shot-2.png",
            "a case folding filesystem would otherwise make these one file"
        );
    }

    #[test]
    fn disambiguation_of_a_maximum_length_name_still_fits() {
        let mut names = BatchNames::new();
        let long = format!("{}.png", "a".repeat(MAX_LEN - 4));
        let first = names.assign(&long);
        let second = names.assign(&long);
        assert_ne!(first, second);
        assert!(
            second.as_str().len() <= MAX_LEN,
            "{}",
            second.as_str().len()
        );
        assert_eq!(second.extension(), Some(".png"));
        assert!(second.as_str().contains("-2."), "{second}");
    }

    #[test]
    fn names_without_an_extension_are_disambiguated_too() {
        let mut names = BatchNames::new();
        assert_eq!(names.assign("README").as_str(), "README");
        assert_eq!(names.assign("README").as_str(), "README-2");
        assert_eq!(names.assign(".bashrc").as_str(), ".bashrc");
        assert_eq!(names.assign(".bashrc").as_str(), ".bashrc-2");
    }

    #[test]
    fn extension_handles_dotfiles_and_multiple_dots() {
        let ext = |n: &str| SafeFileName::new(n).map(|f| f.extension().map(str::to_string));
        assert_eq!(ext("shot.png").unwrap(), Some(".png".to_string()));
        assert_eq!(ext("archive.tar.gz").unwrap(), Some(".gz".to_string()));
        assert_eq!(ext(".bashrc").unwrap(), None);
        assert_eq!(ext("noext").unwrap(), None);
        assert_eq!(ext("trailing.").unwrap(), None);
    }
}
