//! Reading and writing individual configuration values by dotted key.
//!
//! Edits are applied to the raw TOML document rather than to a [`Config`],
//! then validated by parsing the result. Two things fall out of that: an edit
//! can never produce a file Clift would refuse to load, and keys this build
//! does not know about survive the write instead of being quietly deleted:
//! The specification asks for a warning about them, not for their removal.
//!
//! [`Config`]: super::Config

use crate::error::{CliftError, ErrorKind, Remedy, Stage};

/// The value shape a key accepts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ValueKind {
    Text,
    Count,
    /// A TOML boolean. Only `true` and `false` are accepted: a configuration
    /// that takes `yes` and `1` as well eventually has to decide what `on`
    /// means, and there is no answer to that a user could have predicted.
    Flag,
}

/// Keys that are not under `targets.<name>`.
const FIXED_KEYS: &[(&str, ValueKind)] = &[
    ("mode", ValueKind::Text),
    ("default_target", ValueKind::Text),
    ("relay.url", ValueKind::Text),
    ("relay.max_bytes", ValueKind::Text),
    ("relay.ttl", ValueKind::Text),
    ("hotkey.combination", ValueKind::Text),
    ("connection.reuse", ValueKind::Flag),
    ("connection.persist", ValueKind::Text),
    ("defaults.max_file_size", ValueKind::Text),
    ("defaults.max_batch_size", ValueKind::Text),
    ("defaults.max_files", ValueKind::Count),
    ("defaults.retention", ValueKind::Text),
    ("defaults.format", ValueKind::Text),
];

/// Fields a target may carry.
const TARGET_FIELDS: &[(&str, ValueKind)] = &[
    ("ssh_host", ValueKind::Text),
    ("remote_dir", ValueKind::Text),
    ("format", ValueKind::Text),
    ("remote_home", ValueKind::Text),
    ("last_success_at", ValueKind::Text),
];

/// Every settable key, with `<name>` standing in for a target alias.
#[must_use]
pub fn settable_keys() -> Vec<String> {
    let mut keys: Vec<String> = FIXED_KEYS
        .iter()
        .map(|(key, _)| (*key).to_string())
        .collect();
    keys.extend(
        TARGET_FIELDS
            .iter()
            .map(|(field, _)| format!("targets.<name>.{field}")),
    );
    keys.sort();
    keys
}

fn kind_of(key: &str) -> Option<ValueKind> {
    if let Some((_, kind)) = FIXED_KEYS.iter().find(|(name, _)| *name == key) {
        return Some(*kind);
    }
    let parts: Vec<&str> = key.split('.').collect();
    if parts.len() == 3 && parts[0] == "targets" {
        return TARGET_FIELDS
            .iter()
            .find(|(field, _)| *field == parts[2])
            .map(|(_, kind)| *kind);
    }
    None
}

fn unknown_key(key: &str) -> CliftError {
    CliftError::new(
        Stage::Config,
        ErrorKind::Config,
        format!("unknown config key {key:?}"),
    )
    .with_remedy(Remedy::new(
        format!("Valid keys are {}. Try:", settable_keys().join(", ")),
        "clift config get default_target",
    ))
}

/// Reads one value, rendered the way it appears in the file.
///
/// # Errors
/// Fails for an unknown key, and for a key that is not set.
pub fn get(document: &toml::Table, key: &str) -> Result<String, CliftError> {
    if kind_of(key).is_none() {
        return Err(unknown_key(key));
    }

    let mut current = toml::Value::Table(document.clone());
    for part in key.split('.') {
        let Some(table) = current.as_table() else {
            return Err(not_set(key));
        };
        let Some(next) = table.get(part) else {
            return Err(not_set(key));
        };
        current = next.clone();
    }

    Ok(match current {
        toml::Value::String(text) => text,
        other => other.to_string(),
    })
}

fn not_set(key: &str) -> CliftError {
    CliftError::new(
        Stage::Config,
        ErrorKind::Config,
        format!("config key {key:?} is not set"),
    )
    .with_remedy(Remedy::new(
        "Set it with:",
        format!("clift config set {key} <value>"),
    ))
}

/// Writes one value into the document, leaving every other key untouched.
///
/// The caller must parse the result before persisting it: this function only
/// places the value, it does not know whether the document as a whole is
/// coherent.
///
/// # Errors
/// Fails for an unknown key, and for a value that does not fit the key's shape.
pub fn set(document: &mut toml::Table, key: &str, value: &str) -> Result<(), CliftError> {
    let Some(kind) = kind_of(key) else {
        return Err(unknown_key(key));
    };

    let parsed = match kind {
        ValueKind::Text => toml::Value::String(value.to_string()),
        ValueKind::Count => {
            let Ok(number) = value.parse::<i64>() else {
                return Err(CliftError::new(
                    Stage::Config,
                    ErrorKind::Config,
                    format!("config key {key:?} expects a whole number, got {value:?}"),
                ));
            };
            toml::Value::Integer(number)
        }
        ValueKind::Flag => match value {
            "true" => toml::Value::Boolean(true),
            "false" => toml::Value::Boolean(false),
            other => {
                return Err(CliftError::new(
                    Stage::Config,
                    ErrorKind::Config,
                    format!("config key {key:?} expects true or false, got {other:?}"),
                ));
            }
        },
    };

    let parts: Vec<&str> = key.split('.').collect();
    let (last, parents) = parts.split_last().unwrap_or((&"", &[]));

    let mut table = document;
    for part in parents {
        let entry = table
            .entry((*part).to_string())
            .or_insert_with(|| toml::Value::Table(toml::Table::new()));
        let Some(next) = entry.as_table_mut() else {
            return Err(CliftError::new(
                Stage::Config,
                ErrorKind::Config,
                format!("config key {key:?} cannot be set because {part:?} is not a table"),
            ));
        };
        table = next;
    }
    table.insert((*last).to_string(), parsed);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn document(source: &str) -> toml::Table {
        source.parse().unwrap()
    }

    #[test]
    fn reads_values_at_every_level() {
        let doc = document(
            "version = 1\ndefault_target = \"core\"\n[defaults]\nmax_files = 20\n\
             [targets.core]\nssh_host = \"core\"\n[relay]\nurl = \"https://relay.example.com\"\n",
        );
        assert_eq!(get(&doc, "default_target").unwrap(), "core");
        assert_eq!(get(&doc, "defaults.max_files").unwrap(), "20");
        assert_eq!(get(&doc, "targets.core.ssh_host").unwrap(), "core");
        assert_eq!(get(&doc, "relay.url").unwrap(), "https://relay.example.com");
    }

    #[test]
    fn a_flag_takes_only_the_two_words_it_means() {
        let mut doc = document("version = 1\n");
        set(&mut doc, "connection.reuse", "false").unwrap();
        assert_eq!(get(&doc, "connection.reuse").unwrap(), "false");
        set(&mut doc, "connection.persist", "30m").unwrap();
        assert_eq!(get(&doc, "connection.persist").unwrap(), "30m");

        for refused in ["yes", "1", "on", "TRUE"] {
            let error = set(&mut doc, "connection.reuse", refused).unwrap_err();
            assert_eq!(error.exit_code().as_u8(), 20, "{refused}");
        }
        assert_eq!(
            get(&doc, "connection.reuse").unwrap(),
            "false",
            "a rejected value must not have half-written itself"
        );
    }

    #[test]
    fn an_unknown_key_is_rejected_and_the_valid_ones_are_listed() {
        let doc = document("version = 1\n");
        let error = get(&doc, "defaults.max_filez").unwrap_err();
        assert_eq!(error.exit_code().as_u8(), 20);
        let remedy = error.remedy().unwrap();
        assert!(
            remedy.description().contains("defaults.max_files"),
            "the remedy should list the valid keys: {remedy:?}"
        );
        assert!(
            remedy.command().starts_with("clift config get"),
            "the remedy command must be runnable as written: {remedy:?}"
        );

        assert_eq!(
            set(&mut document("version = 1\n"), "nope", "1")
                .unwrap_err()
                .exit_code()
                .as_u8(),
            20
        );
    }

    #[test]
    fn an_unset_key_says_how_to_set_it() {
        let doc = document("version = 1\n");
        let error = get(&doc, "default_target").unwrap_err();
        assert_eq!(error.exit_code().as_u8(), 20);
        assert!(
            error
                .remedy()
                .unwrap()
                .command()
                .contains("clift config set default_target"),
            "{error}"
        );
    }

    #[test]
    fn setting_creates_missing_tables() {
        let mut doc = document("version = 1\n");
        set(&mut doc, "targets.core.ssh_host", "core").unwrap();
        set(&mut doc, "relay.url", "https://relay.example.com").unwrap();
        assert_eq!(get(&doc, "targets.core.ssh_host").unwrap(), "core");
        assert_eq!(get(&doc, "relay.url").unwrap(), "https://relay.example.com");
    }

    #[test]
    fn setting_preserves_unrelated_keys_including_unknown_ones() {
        let mut doc =
            document("version = 1\nexperimental_key = true\n[targets.core]\nssh_host = \"core\"\n");
        set(&mut doc, "default_target", "core").unwrap();
        assert_eq!(
            doc.get("experimental_key").and_then(toml::Value::as_bool),
            Some(true),
            "an edit must not delete keys this build does not recognise"
        );
        assert_eq!(get(&doc, "targets.core.ssh_host").unwrap(), "core");
    }

    #[test]
    fn a_count_key_rejects_non_numbers() {
        let mut doc = document("version = 1\n");
        let error = set(&mut doc, "defaults.max_files", "twenty").unwrap_err();
        assert_eq!(error.exit_code().as_u8(), 20);
        assert!(error.message().contains("whole number"), "{error}");
    }

    #[test]
    fn a_count_key_stores_an_integer_not_a_string() {
        let mut doc = document("version = 1\n");
        set(&mut doc, "defaults.max_files", "5").unwrap();
        assert!(
            doc["defaults"]["max_files"].is_integer(),
            "max_files must round trip as an integer"
        );
    }

    #[test]
    fn every_settable_key_is_also_a_known_schema_key() {
        let known = super::super::schema::all_known_keys();
        for key in settable_keys() {
            assert!(
                known.contains(&key),
                "{key} is settable but is not part of the schema"
            );
        }
    }
}
