//! The specification sanitisation, driven by `tests/fixtures/filenames/cases.toml`.
//!
//! The fixture exists so the acceptance criteria can be checked one case at a
//! time rather than taken on trust, and so a future change to the rules has to
//! confront every shape at once.

// A panic here is a test failure, not a user-facing crash; see clippy.toml.
// `expect` is used in the fixture loader, which is not itself a `#[test]` fn.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use clift_core::domain::{BatchNames, SafeFileName};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
struct Cases {
    case: Vec<Case>,
}

#[derive(Debug, Deserialize)]
struct Case {
    name: String,
    input: String,
    /// The exact result, where it does not depend on the length limit.
    expected: Option<String>,
    /// The extension that must survive truncation.
    ends_with: Option<String>,
}

fn cases() -> Vec<Case> {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/fixtures/filenames/cases.toml"
    );
    let text = std::fs::read_to_string(path).expect("the fixture must be readable");
    let parsed: Cases = toml::from_str(&text).expect("the fixture must be valid TOML");
    assert!(parsed.case.len() >= 20, "the fixture lost cases");
    parsed.case
}

/// The properties every sanitised name must have, whatever it came from.
#[test]
fn no_input_can_produce_a_separator_a_control_character_or_an_oversized_name() {
    for case in cases() {
        let name = SafeFileName::sanitize(&case.input);
        let text = name.as_str();

        assert!(!text.is_empty(), "{}: empty result", case.name);
        assert!(
            !text.contains('/') && !text.contains('\\'),
            "{}: a separator survived: {text:?}",
            case.name
        );
        assert!(
            !text.chars().any(char::is_control),
            "{}: a control character survived: {text:?}",
            case.name
        );
        assert!(
            !text.starts_with('-'),
            "{}: a leading dash survived: {text:?}",
            case.name
        );
        assert!(
            text != "." && text != "..",
            "{}: a traversal entry survived",
            case.name
        );
        assert!(
            text.len() <= clift_core::domain::MAX_FILE_NAME_LEN,
            "{}: {} bytes",
            case.name,
            text.len()
        );
        // The strict constructor is the authority on what a name may be; a
        // sanitised name that it rejects would be a hole in the type.
        assert!(
            SafeFileName::new(text).is_ok(),
            "{}: new() rejects the sanitised name {text:?}",
            case.name
        );
    }
}

#[test]
fn each_case_produces_the_result_the_fixture_records() {
    for case in cases() {
        let name = SafeFileName::sanitize(&case.input);
        if let Some(expected) = &case.expected {
            assert_eq!(name.as_str(), expected, "{}", case.name);
        }
        if let Some(suffix) = &case.ends_with {
            assert!(
                name.as_str().ends_with(suffix),
                "{}: the extension did not survive truncation: {name}",
                case.name
            );
        }
    }
}

/// Same-name files are isolated by the batch directory, and within a
/// batch nothing may overwrite anything.
#[test]
fn every_fixture_name_stays_distinct_inside_one_batch() {
    let cases = cases();
    let mut names = BatchNames::new();
    let mut assigned = Vec::new();

    // Twice through, so that every case also collides with itself.
    for _ in 0..2 {
        for case in &cases {
            assigned.push(names.assign(&case.input).as_str().to_lowercase());
        }
    }

    let count = assigned.len();
    assigned.sort();
    assigned.dedup();
    assert_eq!(
        assigned.len(),
        count,
        "two files in one batch were given the same name"
    );
}
