//! The serde shape of `config.toml`, plus detection of unknown keys.
//!
//! `deny_unknown_fields` is deliberately **not** used. The specification asks for a
//! warning on a misspelled key, not a refusal to start: a user whose config
//! gained a stray field after an upgrade must still be able to paste. So the
//! document is walked once against the known key set to collect warnings, and
//! then deserialized with serde, which ignores what it does not recognise.

use serde::Deserialize;
use std::collections::BTreeMap;

/// Highest `version` this build understands.
pub const SUPPORTED_VERSION: u32 = 1;

#[derive(Debug, Default, Deserialize)]
pub(crate) struct RawConfig {
    // `version` is deliberately absent: it is consumed by the migration layer
    // before typed parsing, so having it here too would invite the two from
    // disagreeing.
    pub mode: Option<String>,
    pub default_target: Option<String>,
    pub defaults: Option<RawDefaults>,
    pub targets: Option<BTreeMap<String, RawTarget>>,
    pub relay: Option<RawRelay>,
    pub hotkey: Option<RawHotkey>,
    pub connection: Option<RawConnection>,
}

#[derive(Debug, Default, Deserialize)]
pub(crate) struct RawConnection {
    pub reuse: Option<bool>,
    pub persist: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
pub(crate) struct RawHotkey {
    pub combination: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
pub(crate) struct RawRelay {
    pub url: Option<String>,
    pub max_bytes: Option<String>,
    pub ttl: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
pub(crate) struct RawDefaults {
    pub max_file_size: Option<String>,
    pub max_batch_size: Option<String>,
    pub max_files: Option<u32>,
    pub retention: Option<String>,
    pub format: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct RawTarget {
    pub ssh_host: String,
    pub remote_dir: Option<String>,
    pub format: Option<String>,
    pub remote_home: Option<String>,
    pub last_success_at: Option<String>,
}

const ROOT_KEYS: &[&str] = &[
    "version",
    "mode",
    "default_target",
    "defaults",
    "targets",
    "relay",
    "hotkey",
    "connection",
];
const DEFAULTS_KEYS: &[&str] = &[
    "max_file_size",
    "max_batch_size",
    "max_files",
    "retention",
    "format",
];
const TARGET_KEYS: &[&str] = &[
    "ssh_host",
    "remote_dir",
    "format",
    "remote_home",
    "last_success_at",
];
// No key here may hold a credential, and none does: a relay is not
// authenticated, by design. The test below enforces that the schema
// offers nowhere to put one.
const RELAY_KEYS: &[&str] = &["url", "max_bytes", "ttl"];
// One key, and it names a key combination rather than a cryptographic one.
// Spelled out in full for that reason: a section called `hotkey` with a field
// called `key` reads, at a glance, like the one thing this schema must never
// have anywhere to put.
const HOTKEY_KEYS: &[&str] = &["combination"];
// Whether to reuse an SSH connection, and how long an idle one is kept. Not a
// place for anything about *how* to connect: authentication stays entirely
// with the user's own SSH configuration.
const CONNECTION_KEYS: &[&str] = &["reuse", "persist"];

/// Every key the schema defines, as dotted paths, for auditing purposes.
#[must_use]
pub fn all_known_keys() -> Vec<String> {
    let mut keys: Vec<String> = ROOT_KEYS.iter().map(|k| (*k).to_string()).collect();
    keys.extend(DEFAULTS_KEYS.iter().map(|k| format!("defaults.{k}")));
    keys.extend(TARGET_KEYS.iter().map(|k| format!("targets.<name>.{k}")));
    keys.extend(RELAY_KEYS.iter().map(|k| format!("relay.{k}")));
    keys.extend(HOTKEY_KEYS.iter().map(|k| format!("hotkey.{k}")));
    keys.extend(CONNECTION_KEYS.iter().map(|k| format!("connection.{k}")));
    keys
}

/// Collects the dotted path of every key the schema does not define.
pub(crate) fn unknown_keys(document: &toml::Table) -> Vec<String> {
    let mut found = Vec::new();
    collect(document, "", ROOT_KEYS, &mut found);

    if let Some(toml::Value::Table(defaults)) = document.get("defaults") {
        collect(defaults, "defaults", DEFAULTS_KEYS, &mut found);
    }
    if let Some(toml::Value::Table(targets)) = document.get("targets") {
        for (name, value) in targets {
            if let toml::Value::Table(target) = value {
                collect(target, &format!("targets.{name}"), TARGET_KEYS, &mut found);
            }
        }
    }
    if let Some(toml::Value::Table(relay)) = document.get("relay") {
        collect(relay, "relay", RELAY_KEYS, &mut found);
    }
    if let Some(toml::Value::Table(hotkey)) = document.get("hotkey") {
        collect(hotkey, "hotkey", HOTKEY_KEYS, &mut found);
    }
    if let Some(toml::Value::Table(connection)) = document.get("connection") {
        collect(connection, "connection", CONNECTION_KEYS, &mut found);
    }

    found.sort();
    found
}

fn collect(table: &toml::Table, prefix: &str, known: &[&str], found: &mut Vec<String>) {
    for key in table.keys() {
        if known.contains(&key.as_str()) {
            continue;
        }
        found.push(if prefix.is_empty() {
            key.clone()
        } else {
            format!("{prefix}.{key}")
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The schema must offer nowhere to put a secret. Making it impossible to
    /// write a credential is stronger than remembering not to.
    #[test]
    fn schema_has_no_field_that_could_hold_a_credential() {
        const FORBIDDEN: &[&str] = &[
            "password",
            "passwd",
            "token",
            "secret",
            "credential",
            "cookie",
            "identity",
            "identityfile",
            "private",
            "passphrase",
            "apikey",
            "api_key",
            "auth",
        ];
        for key in all_known_keys() {
            let leaf = key.rsplit('.').next().unwrap_or(&key).to_ascii_lowercase();
            for forbidden in FORBIDDEN {
                assert!(
                    !leaf.contains(forbidden),
                    "config key {key:?} looks like it could hold a credential"
                );
            }
        }
    }

    /// `[integrations.wezterm]` is in here on purpose: it is a section earlier
    /// versions wrote and this one no longer defines. It must come back as one
    /// unknown key, which warns, rather than as a parse failure.
    #[test]
    fn finds_unknown_keys_at_every_level() {
        let document: toml::Table = r#"
            version = 1
            typo_at_root = true
            [defaults]
            max_files = 20
            max_filez = 21
            [targets.core]
            ssh_host = "core"
            sshhost = "core"
            [integrations.wezterm]
            paste_mode = "dedicated"
            [relay]
            url = "https://relay.example.com"
            urls = "x"
            [hotkey]
            combination = "cmd+shift+v"
            combo = "x"
            [connection]
            reuse = true
            resue = false
        "#
        .parse()
        .unwrap();
        assert_eq!(
            unknown_keys(&document),
            vec![
                "connection.resue".to_string(),
                "defaults.max_filez".to_string(),
                "hotkey.combo".to_string(),
                "integrations".to_string(),
                "relay.urls".to_string(),
                "targets.core.sshhost".to_string(),
                "typo_at_root".to_string(),
            ]
        );
    }

    #[test]
    fn a_correct_document_has_no_unknown_keys() {
        let document: toml::Table = r#"
            version = 1
            mode = "universal"
            default_target = "core"
            [defaults]
            max_file_size = "50MiB"
            max_batch_size = "100MiB"
            max_files = 20
            retention = "24h"
            format = "instruction"
            [targets.core]
            ssh_host = "core"
            remote_dir = "~/.cache/clift/inbox"
            format = "instruction"
            remote_home = "/home/dev"
            last_success_at = "2026-08-30T12:00:00Z"
            [relay]
            url = "https://relay.example.com"
            max_bytes = "8MiB"
            ttl = "5m"
            [hotkey]
            combination = "cmd+shift+v"
            [connection]
            reuse = true
            persist = "10m"
        "#
        .parse()
        .unwrap();
        assert!(unknown_keys(&document).is_empty());
    }
}
