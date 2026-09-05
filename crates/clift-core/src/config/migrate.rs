//! Forward-only migration of the configuration document.
//!
//! Migrations run on the raw TOML document, before typed parsing: a field that
//! changed shape between versions cannot be deserialized into today's structs
//! in the first place. Each step goes from version *n* to *n+1* and there is no
//! reverse direction: downgrading would have to guess what to drop.

use super::schema::SUPPORTED_VERSION;
use crate::error::{CliftError, ErrorKind, Remedy, Stage};

/// One step of the chain, from `from_version` to `from_version + 1`.
struct Migration {
    from_version: u32,
    apply: fn(&mut toml::Table),
    note: &'static str,
}

/// The chain, in ascending order. Adding a schema version means appending one
/// entry here and raising [`SUPPORTED_VERSION`].
const MIGRATIONS: &[Migration] = &[Migration {
    from_version: 0,
    apply: add_version_key,
    note: "config had no 'version' key; recorded it as version 1",
}];

/// A document with no `version` key predates versioning. Hand-written configs
/// look like this, so stamping the version is a real migration rather than a
/// placeholder.
fn add_version_key(document: &mut toml::Table) {
    document.insert("version".to_string(), toml::Value::Integer(1));
}

/// What a migration run changed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MigrationOutcome {
    pub from_version: u32,
    pub to_version: u32,
    pub notes: Vec<String>,
}

impl MigrationOutcome {
    #[must_use]
    pub fn migrated(&self) -> bool {
        self.from_version != self.to_version
    }
}

/// Reads the declared version, treating an absent key as version 0.
///
/// # Errors
/// Fails if `version` is present but is not a non-negative integer.
pub fn declared_version(document: &toml::Table) -> Result<u32, CliftError> {
    match document.get("version") {
        None => Ok(0),
        Some(toml::Value::Integer(value)) if *value >= 0 => {
            u32::try_from(*value).map_err(|error| {
                CliftError::new(
                    Stage::Config,
                    ErrorKind::Config,
                    format!("config field 'version' is out of range: {value}"),
                )
                .with_source(error)
            })
        }
        Some(other) => Err(CliftError::new(
            Stage::Config,
            ErrorKind::Config,
            format!(
                "config field 'version' must be a non-negative integer, found {}",
                other.type_str()
            ),
        )),
    }
}

/// Brings a document up to [`SUPPORTED_VERSION`], applying each step in order.
///
/// # Errors
/// Refuses a document newer than this build understands: its fields may
/// have changed meaning, so reading them with today's rules would be a guess.
pub fn migrate_to_current(document: &mut toml::Table) -> Result<MigrationOutcome, CliftError> {
    // An empty document is a first run, not a legacy file: there is nothing to
    // bring forward, and warning about it would greet every new user with a
    // complaint about a file they have never written.
    if document.is_empty() {
        return Ok(MigrationOutcome {
            from_version: SUPPORTED_VERSION,
            to_version: SUPPORTED_VERSION,
            notes: Vec::new(),
        });
    }

    let from_version = declared_version(document)?;

    if from_version > SUPPORTED_VERSION {
        return Err(CliftError::new(
            Stage::Config,
            ErrorKind::Config,
            format!(
                "config version {from_version} is newer than this build supports (version {SUPPORTED_VERSION})"
            ),
        )
        .with_remedy(Remedy::new("Upgrade Clift, then try again:", "clift --version")));
    }

    let mut notes = Vec::new();
    let mut version = from_version;
    while version < SUPPORTED_VERSION {
        let Some(migration) = MIGRATIONS
            .iter()
            .find(|candidate| candidate.from_version == version)
        else {
            return Err(CliftError::new(
                Stage::Config,
                ErrorKind::Internal,
                format!("no migration is registered from config version {version}"),
            ));
        };
        (migration.apply)(document);
        notes.push(migration.note.to_string());
        version += 1;
    }

    Ok(MigrationOutcome {
        from_version,
        to_version: version,
        notes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_chain_is_ascending_and_has_no_gaps_or_duplicates() {
        let mut expected = 0;
        for migration in MIGRATIONS {
            assert_eq!(
                migration.from_version, expected,
                "migration chain has a gap or is out of order"
            );
            expected += 1;
        }
        assert_eq!(
            expected, SUPPORTED_VERSION,
            "the chain must reach the supported version"
        );
    }

    #[test]
    fn every_migration_moves_exactly_one_version_forward() {
        for migration in MIGRATIONS {
            let mut document = toml::Table::new();
            document.insert(
                "version".to_string(),
                toml::Value::Integer(i64::from(migration.from_version)),
            );
            (migration.apply)(&mut document);
            let after = declared_version(&document).unwrap();
            assert_eq!(
                after,
                migration.from_version + 1,
                "migration from {} did not land on the next version",
                migration.from_version
            );
        }
    }

    #[test]
    fn a_document_without_a_version_is_migrated_to_v1() {
        let mut document: toml::Table = "default_target = \"core\"".parse().unwrap();
        let outcome = migrate_to_current(&mut document).unwrap();
        assert!(outcome.migrated());
        assert_eq!(outcome.from_version, 0);
        assert_eq!(outcome.to_version, 1);
        assert_eq!(outcome.notes.len(), 1);
        assert_eq!(declared_version(&document).unwrap(), 1);
        assert_eq!(
            document.get("default_target").and_then(toml::Value::as_str),
            Some("core"),
            "migration must not lose existing data"
        );
    }

    #[test]
    fn an_empty_document_needs_no_migration_and_no_warning() {
        let mut document = toml::Table::new();
        let outcome = migrate_to_current(&mut document).unwrap();
        assert!(!outcome.migrated());
        assert!(outcome.notes.is_empty());
        assert!(document.is_empty());
    }

    #[test]
    fn a_current_document_is_left_alone() {
        let mut document: toml::Table = "version = 1\ndefault_target = \"core\"".parse().unwrap();
        let before = document.clone();
        let outcome = migrate_to_current(&mut document).unwrap();
        assert!(!outcome.migrated());
        assert!(outcome.notes.is_empty());
        assert_eq!(document, before);
    }

    #[test]
    fn a_newer_document_is_refused_and_never_downgraded() {
        let mut document: toml::Table = "version = 999".parse().unwrap();
        let err = migrate_to_current(&mut document).unwrap_err();
        assert_eq!(err.exit_code().as_u8(), 20);
        assert_eq!(
            declared_version(&document).unwrap(),
            999,
            "a refused document must be left untouched, not rewritten backwards"
        );
    }

    #[test]
    fn no_migration_runs_backwards() {
        for migration in MIGRATIONS {
            assert!(
                migration.from_version < SUPPORTED_VERSION,
                "a migration starting at or above the supported version would run backwards"
            );
        }
    }

    #[test]
    fn a_non_integer_version_is_a_config_error() {
        for source in ["version = \"1\"", "version = 1.5", "version = true"] {
            let document: toml::Table = source.parse().unwrap();
            let err = declared_version(&document).unwrap_err();
            assert_eq!(err.exit_code().as_u8(), 20, "{source}");
        }
    }
}
