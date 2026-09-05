//! Fixture-driven checks for the specification's strict-but-forgiving parsing rules.
//!
//! Fixtures live at the repository root so that integration tests, the CLI and
//! future tooling all read the same files rather than each keeping a copy.

// A panic here is a test failure, not a user-facing crash; see clippy.toml.
#![allow(clippy::unwrap_used)]

use clift_core::config;
use std::path::PathBuf;

fn fixture(name: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/config")
        .join(name);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()))
}

#[test]
fn parses_a_full_configuration() {
    let loaded = config::parse(&fixture("valid_full.toml")).unwrap();
    assert!(loaded.warnings.is_empty(), "{:?}", loaded.warnings);

    let cfg = loaded.config;
    assert_eq!(cfg.version(), 1);
    assert_eq!(cfg.default_target().unwrap().as_str(), "core");
    assert_eq!(cfg.targets().len(), 2);

    let defaults = cfg.defaults();
    assert_eq!(defaults.limits().max_file_size(), 50 * 1024 * 1024);
    assert_eq!(defaults.limits().max_batch_size(), 100 * 1024 * 1024);
    assert_eq!(defaults.limits().max_files(), 20);
    assert_eq!(defaults.retention().as_secs(), 24 * 60 * 60);
    assert_eq!(defaults.format(), config::Format::Instruction);

    let core = cfg
        .target(cfg.default_target().unwrap())
        .expect("core target");
    assert_eq!(core.ssh_host(), "core");
    assert_eq!(core.remote_dir(), "~/.cache/clift/inbox");
    assert_eq!(core.remote_home().unwrap().as_str(), "/home/dev");
    assert_eq!(core.last_success_at(), Some("2026-08-30T12:00:00Z"));
}

#[test]
fn a_target_without_optional_fields_uses_the_default_inbox() {
    let loaded = config::parse(&fixture("valid_full.toml")).unwrap();
    let hk = loaded
        .config
        .targets()
        .values()
        .find(|t| t.ssh_host() == "hk")
        .expect("hk target");
    assert_eq!(hk.remote_dir(), config::DEFAULT_REMOTE_DIR);
    assert_eq!(hk.format(), None, "target inherits the global default");
    assert_eq!(hk.remote_home(), None);
}

#[test]
fn a_missing_config_file_is_an_empty_config_not_an_error() {
    let loaded = config::parse("").unwrap();
    assert!(loaded.config.targets().is_empty());
    assert!(loaded.config.default_target().is_none());
    assert!(loaded.warnings.is_empty());
    assert_eq!(
        loaded.config.defaults().limits().max_files(),
        20,
        "defaults still apply to an empty config"
    );
}

#[test]
fn a_config_with_only_a_version_is_valid() {
    let loaded = config::parse(&fixture("minimal.toml")).unwrap();
    assert!(loaded.config.targets().is_empty());
    assert!(loaded.warnings.is_empty());
}

#[test]
fn unknown_fields_warn_and_do_not_stop_parsing() {
    let loaded = config::parse(&fixture("unknown_fields.toml")).unwrap();

    assert_eq!(
        loaded.config.default_target().unwrap().as_str(),
        "core",
        "known fields must still be applied"
    );
    assert_eq!(loaded.warnings.len(), 4, "{:?}", loaded.warnings);
    for key in [
        "retention_days",
        "defaults.max_filez",
        "targets.core.sshhost",
        // A section an older version wrote and this one does not define: it
        // warns once, as a whole, and does not stop the file being read.
        "integrations",
    ] {
        assert!(
            loaded.warnings.iter().any(|w| w.contains(key)),
            "no warning mentions {key}: {:?}",
            loaded.warnings
        );
    }
}

#[test]
fn a_wrong_type_fails_with_exit_code_20_and_names_the_field() {
    let err = config::parse(&fixture("type_error.toml")).unwrap_err();
    assert_eq!(err.exit_code().as_u8(), 20);
    let chain = err.cause_chain().join(" | ");
    assert!(
        chain.contains("max_files"),
        "error does not name the offending field: {chain}"
    );
}

#[test]
fn malformed_toml_fails_with_exit_code_20() {
    let err = config::parse("version = ").unwrap_err();
    assert_eq!(err.exit_code().as_u8(), 20);
    assert!(err.message().contains("TOML"), "{}", err.message());
}

#[test]
fn a_newer_schema_version_is_refused_rather_than_downgraded() {
    let err = config::parse(&fixture("future_version.toml")).unwrap_err();
    assert_eq!(err.exit_code().as_u8(), 20);
    assert!(err.message().contains("999"), "{}", err.message());
    let remedy = err.remedy().expect("a refusal must say how to recover");
    assert!(
        remedy.description().to_lowercase().contains("upgrade"),
        "{}",
        remedy.description()
    );
}

#[test]
fn default_target_must_reference_a_configured_target() {
    let err = config::parse(&fixture("dangling_default_target.toml")).unwrap_err();
    assert_eq!(err.exit_code().as_u8(), 20);
    assert!(err.message().contains("typo"), "{}", err.message());
}

#[test]
fn invalid_values_are_rejected_with_exit_code_20() {
    let cases = [
        ("[defaults]\nmax_file_size = \"50TiB\"\n", "unit"),
        ("[defaults]\nretention = \"24\"\n", "unit"),
        ("[defaults]\nformat = \"markdown\"\n", "instruction"),
        ("[defaults]\nmax_files = 0\n", "max_files"),
        (
            "[defaults]\nmax_file_size = \"200MiB\"\nmax_batch_size = \"100MiB\"\n",
            "max_batch_size",
        ),
        ("[targets.core]\nssh_host = \"\"\n", "ssh_host"),
        ("[targets.core]\nssh_host = \"a b\"\n", "ssh_host"),
        (
            "[targets.core]\nssh_host = \"core\"\nremote_home = \"relative/path\"\n",
            "absolute",
        ),
    ];
    for (source, needle) in cases {
        let err = config::parse(source)
            .err()
            .unwrap_or_else(|| panic!("accepted invalid config: {source:?}"));
        assert_eq!(err.exit_code().as_u8(), 20, "wrong code for {source:?}");
        let chain = err.cause_chain().join(" | ");
        assert!(
            chain.contains(needle),
            "error for {source:?} does not mention {needle:?}: {chain}"
        );
    }
}

/// The schema must offer nowhere to write a credential. Making that impossible
/// is stronger than remembering not to.
///
/// The check is on the leaf name rather than the whole dotted path, because a
/// leaf is the only part that holds a value: `[hotkey]` is a table, and a table
/// cannot store anything. That distinction was drawn when `hotkey.combination`
/// was added -- the earlier version rejected the whole path, and "hotkey"
/// contains "key". The needle list itself is unchanged and still includes
/// "key", so a field actually called `key` is still refused.
#[test]
fn the_schema_offers_nowhere_to_store_a_secret() {
    let keys = config::schema::all_known_keys();
    assert!(!keys.is_empty());

    // A path that other paths hang off is a table, and a table holds no value.
    let tables: Vec<String> = keys
        .iter()
        .filter_map(|key| key.rsplit_once('.').map(|(prefix, _)| prefix.to_string()))
        .collect();

    let mut checked = 0;
    for key in &keys {
        if tables.iter().any(|table| table == key) {
            continue;
        }
        checked += 1;
        let leaf = key.rsplit('.').next().unwrap_or(key).to_ascii_lowercase();
        for forbidden in ["password", "token", "secret", "identity", "key", "cookie"] {
            assert!(
                !leaf.contains(forbidden),
                "config key {key:?} could hold a credential"
            );
        }
    }
    // Skipping tables must not be a way to skip everything.
    assert!(checked > 10, "only {checked} keys were actually checked");

    // And the exemption is not a hole somebody can widen by accident: exactly
    // one section is allowed to have "key" in its name, and it is this one.
    let sections: Vec<&String> = keys
        .iter()
        .filter(|key| {
            key.rsplit_once('.')
                .is_some_and(|(prefix, _)| prefix.to_ascii_lowercase().contains("key"))
        })
        .collect();
    assert_eq!(
        sections,
        vec![&"hotkey.combination".to_string()],
        "a new section with \"key\" in its name needs the same scrutiny this one got"
    );
}
