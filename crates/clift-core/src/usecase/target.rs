//! Managing the list of configured targets.
//!
//! Every function here takes a configuration and returns a new one. Nothing is
//! written, nothing is connected to: the caller decides when the file changes,
//! which is what keeps a rejected argument from leaving a half-edited file
//! behind.

use crate::config::{Config, DEFAULT_REMOTE_DIR, Target};
use crate::domain::TargetName;
use crate::error::{CliftError, ErrorKind, Remedy, Stage};

/// One row of `clift target list`.
///
/// A deliberately small set of fields. There is no place here for a key path or
/// anything else that must not be printed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TargetSummary {
    pub name: String,
    pub ssh_host: String,
    pub is_default: bool,
    pub last_success_at: Option<String>,
}

/// Every configured target, in the order the configuration stores them.
#[must_use]
pub fn list(config: &Config) -> Vec<TargetSummary> {
    config
        .targets()
        .iter()
        .map(|(name, target)| TargetSummary {
            name: name.as_str().to_string(),
            ssh_host: target.ssh_host().to_string(),
            is_default: config.default_target() == Some(name),
            last_success_at: target.last_success_at().map(str::to_string),
        })
        .collect()
}

/// Adds a target for an SSH alias, without contacting it.
///
/// Verification is `setup`'s job. This exists for the user who already knows
/// their host works and wants a second name for it, so it records what it was
/// told and nothing more: no `remote_home`, no `last_success_at`, because
/// neither has been established.
///
/// # Errors
/// Refuses an invalid name and refuses to overwrite an existing target.
pub fn add(config: &Config, name: &str, ssh_host: Option<&str>) -> Result<Config, CliftError> {
    let name = parse_name(name)?;
    if config.target(&name).is_some() {
        return Err(already_exists(&name));
    }
    let host = ssh_host.unwrap_or_else(|| name.as_str());
    let mut next = config.with_target(name.clone(), Target::new(host, DEFAULT_REMOTE_DIR));
    if next.default_target().is_none() {
        next = next.with_default_target(name);
    }
    Ok(next)
}

/// Makes an existing target the default.
///
/// # Errors
/// Fails when the target does not exist, listing what does.
pub fn use_default(config: &Config, name: &str) -> Result<Config, CliftError> {
    let name = parse_name(name)?;
    require_existing(config, &name)?;
    Ok(config.with_default_target(name))
}

/// Renames a target, keeping everything it had learned.
///
/// # Errors
/// Fails when the old name does not exist, when the new one is not a valid
/// target name, and when the new name is already taken.
pub fn rename(config: &Config, from: &str, to: &str) -> Result<Config, CliftError> {
    let from = parse_name(from)?;
    let to = parse_name(to)?;
    let existing = require_existing(config, &from)?.clone();
    if from == to {
        return Ok(config.clone());
    }
    if config.target(&to).is_some() {
        return Err(already_exists(&to));
    }
    Ok(config.with_renamed_target(&from, to, existing))
}

/// What removing a target leaves behind.
///
/// Clift does not delete remote files as a side effect of forgetting a name:
/// the attachments are the user's, and the command they typed was about a local
/// configuration entry. The location is reported so that "leave it" and "clean
/// it up" are both easy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Removal {
    pub config: Config,
    pub ssh_host: String,
    pub remote_dir: String,
    /// True when the removed target was the default and there is now none.
    pub default_cleared: bool,
}

/// Removes a target from the configuration, leaving its remote inbox alone.
///
/// # Errors
/// Fails when the target does not exist.
pub fn remove(config: &Config, name: &str) -> Result<Removal, CliftError> {
    let name = parse_name(name)?;
    let existing = require_existing(config, &name)?;
    let ssh_host = existing.ssh_host().to_string();
    let remote_dir = existing.remote_dir().to_string();
    let was_default = config.default_target() == Some(&name);

    Ok(Removal {
        config: config.without_target(&name),
        ssh_host,
        remote_dir,
        default_cleared: was_default,
    })
}

/// The target one send goes to.
///
/// Two sources, in this order:
///
/// 1. `--to`, which is also how a key binding carries a fixed target: the
///    target the user wrote in their own key definition arrives as `--to`, so
///    the two are one channel and neither can shadow the other;
/// 2. the default target they set with `clift target use`.
///
/// What matters most is what is *not* here. There is no step that reads a pane
/// title, inspects a foreground process, parses an `ssh` command line or picks
/// "the host used most recently". **When it cannot be decided, this fails**;
/// guessing is how an attachment reaches the wrong machine.
///
/// # Errors
/// Fails with exit code 21 when no target can be determined, listing what is
/// configured so the user can choose. Fails with exit code 20 when a named
/// target does not exist.
pub fn resolve_send_target<'a>(
    config: &'a Config,
    requested: Option<&str>,
) -> Result<(TargetName, &'a Target), CliftError> {
    if let Some(name) = requested {
        let name = parse_name(name)?;
        let target = require_existing(config, &name)?;
        return Ok((name, target));
    }

    if let Some(name) = config.default_target()
        && let Some(target) = config.target(name)
    {
        return Ok((name.clone(), target));
    }

    let known: Vec<&str> = config.targets().keys().map(TargetName::as_str).collect();
    let detail = if known.is_empty() {
        "no targets are configured".to_string()
    } else {
        format!("configured targets: {}", known.join(", "))
    };
    Err(CliftError::new(
        Stage::TargetResolution,
        ErrorKind::AmbiguousTarget,
        format!("no target was given and there is no default; {detail}"),
    )
    .with_remedy(if known.is_empty() {
        Remedy::new("Set a host up first:", "clift setup <ssh-host>")
    } else {
        Remedy::new("Name the one you meant:", "clift send --to <name> <file>")
    }))
}

fn parse_name(value: &str) -> Result<TargetName, CliftError> {
    TargetName::new(value).map_err(|error| error.into_clift(Stage::Config, ErrorKind::Config))
}

fn require_existing<'a>(config: &'a Config, name: &TargetName) -> Result<&'a Target, CliftError> {
    config.target(name).ok_or_else(|| {
        let known: Vec<&str> = config.targets().keys().map(TargetName::as_str).collect();
        let detail = if known.is_empty() {
            "no targets are configured".to_string()
        } else {
            format!("configured targets: {}", known.join(", "))
        };
        CliftError::new(
            Stage::Config,
            ErrorKind::Config,
            format!("there is no target called {name}; {detail}"),
        )
        .with_remedy(Remedy::new("See what is configured:", "clift target list"))
    })
}

fn already_exists(name: &TargetName) -> CliftError {
    CliftError::new(
        Stage::Config,
        ErrorKind::Config,
        format!("a target called {name} already exists"),
    )
    .with_remedy(Remedy::new(
        "Pick another name, or rename the existing one:",
        format!("clift target rename {name} <new-name>"),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn configured() -> Config {
        let config = add(&Config::default(), "core", None).unwrap();
        add(&config, "laptop", Some("my-laptop")).unwrap()
    }

    #[test]
    fn the_first_target_added_becomes_the_default_and_later_ones_do_not() {
        let config = configured();
        assert_eq!(
            config.default_target().map(TargetName::as_str),
            Some("core")
        );
    }

    #[test]
    fn an_alias_defaults_to_the_name_but_can_differ_from_it() {
        let config = configured();
        let rows = list(&config);
        let core = rows.iter().find(|row| row.name == "core").unwrap();
        let laptop = rows.iter().find(|row| row.name == "laptop").unwrap();
        assert_eq!(core.ssh_host, "core");
        assert_eq!(laptop.ssh_host, "my-laptop");
        assert!(core.is_default);
        assert!(!laptop.is_default);
        assert_eq!(
            core.last_success_at, None,
            "adding a target proves nothing about it"
        );
    }

    #[test]
    fn adding_a_name_that_is_taken_is_refused_rather_than_silently_replacing() {
        let error = add(&configured(), "core", Some("elsewhere")).expect_err("core exists");
        assert_eq!(error.exit_code().as_u8(), 20);
        assert!(error.to_string().contains("already exists"), "{error}");
    }

    /// Acceptance 2: the newtype's invariant is the gate, and it is checked
    /// before anything is written.
    #[test]
    fn a_new_name_with_a_control_character_or_a_separator_is_refused() {
        for bad in ["with\u{7}bell", "with\nnewline", "with/slash", "", "  "] {
            let error = rename(&configured(), "core", bad)
                .err()
                .unwrap_or_else(|| panic!("{bad:?} should be refused"));
            assert_eq!(error.exit_code().as_u8(), 20, "{bad:?}");
        }
    }

    #[test]
    fn renaming_carries_the_target_and_the_default_with_it() {
        let config = rename(&configured(), "core", "work").unwrap();
        assert!(config.target(&TargetName::new("core").unwrap()).is_none());
        assert_eq!(
            config
                .target(&TargetName::new("work").unwrap())
                .map(clift_core_ssh_host),
            Some("core".to_string())
        );
        assert_eq!(
            config.default_target().map(TargetName::as_str),
            Some("work"),
            "the user renamed the target, they did not stop using it"
        );
    }

    #[test]
    fn renaming_onto_an_existing_name_is_refused() {
        assert!(rename(&configured(), "core", "laptop").is_err());
    }

    #[test]
    fn renaming_a_target_to_its_own_name_changes_nothing() {
        let before = configured();
        assert_eq!(rename(&before, "core", "core").unwrap(), before);
    }

    /// Acceptance 3: the entry goes, the remote files stay, and the caller is
    /// told where they are.
    #[test]
    fn removing_a_target_reports_where_its_inbox_still_is() {
        let removal = remove(&configured(), "core").unwrap();
        assert_eq!(removal.ssh_host, "core");
        assert_eq!(removal.remote_dir, DEFAULT_REMOTE_DIR);
        assert!(removal.default_cleared);
        assert!(
            removal
                .config
                .target(&TargetName::new("core").unwrap())
                .is_none()
        );
    }

    /// Removing the default must not promote another target. Clift
    /// choosing where the next attachment goes is the failure this prevents.
    #[test]
    fn removing_the_default_leaves_no_default_rather_than_picking_one() {
        let removal = remove(&configured(), "core").unwrap();
        assert_eq!(removal.config.default_target(), None);
        assert_eq!(removal.config.targets().len(), 1);
    }

    #[test]
    fn every_operation_on_an_unknown_target_says_what_is_configured() {
        for error in [
            use_default(&configured(), "nope").unwrap_err(),
            rename(&configured(), "nope", "other").unwrap_err(),
            remove(&configured(), "nope").unwrap_err(),
        ] {
            assert_eq!(error.exit_code().as_u8(), 20);
            assert!(error.to_string().contains("core, laptop"), "{error}");
        }
    }

    /// An explicit name wins, the default answers when there is one,
    /// and anything else is a refusal rather than a guess.
    #[test]
    fn a_send_target_is_named_defaulted_or_refused() {
        let config = configured();

        let (name, target) = resolve_send_target(&config, Some("laptop")).unwrap();
        assert_eq!(name.as_str(), "laptop");
        assert_eq!(target.ssh_host(), "my-laptop");

        let (name, _) = resolve_send_target(&config, None).unwrap();
        assert_eq!(
            name.as_str(),
            "core",
            "the default answers when nothing is named"
        );

        let error = resolve_send_target(&config, Some("nope")).unwrap_err();
        assert_eq!(error.exit_code().as_u8(), 20);
    }

    /// A configured target that is not the default does not become one by
    /// being the only one left. The specification step 4 says "the user's explicitly set
    /// default", and nothing was set here.
    #[test]
    fn without_a_default_the_send_is_refused_rather_than_guessed() {
        let config = remove(&configured(), "core").unwrap().config;
        assert_eq!(config.targets().len(), 1);
        assert_eq!(
            config.default_target(),
            None,
            "removing the default cleared it"
        );

        let error = resolve_send_target(&config, None).expect_err("nothing was chosen");
        assert_eq!(error.exit_code().as_u8(), 21);
        assert!(
            error.to_string().contains("laptop"),
            "the refusal must say what there is to choose from: {error}"
        );
    }

    /// Never the most recently used host.
    ///
    /// The configuration records when each target last worked, which makes
    /// "the one you used most recently" trivially available -- and that is
    /// exactly the thing the specification forbids. This test exists because the
    /// temptation is real and the failure it prevents is an attachment on
    /// somebody else's machine.
    #[test]
    fn the_most_recently_used_host_is_never_chosen() {
        let config = add(&Config::default(), "alpha", None).unwrap();
        let config = add(&config, "beta", None).unwrap();

        // Both have a last-success time, and beta's is later.
        let alpha = config
            .target(&TargetName::new("alpha").unwrap())
            .unwrap()
            .clone()
            .with_last_success_at("2026-08-01T00:00:00Z");
        let beta = config
            .target(&TargetName::new("beta").unwrap())
            .unwrap()
            .clone()
            .with_last_success_at("2026-08-30T00:00:00Z");
        let config = config
            .with_target(TargetName::new("alpha").unwrap(), alpha)
            .with_target(TargetName::new("beta").unwrap(), beta);

        // And no default, because the first `add` made alpha the default.
        let config = config.without_target(&TargetName::new("alpha").unwrap());
        let config = add(&config, "alpha", None).unwrap();
        let config = config.without_target(&TargetName::new("alpha").unwrap());
        assert_eq!(config.default_target(), None);
        assert_eq!(config.targets().len(), 1);

        // One target left, with a recorded success, and still no default: the
        // answer is a refusal, not that target.
        let error = resolve_send_target(&config, None)
            .expect_err("a recorded success is not the user choosing a default");
        assert_eq!(error.exit_code().as_u8(), 21);
    }

    #[test]
    fn with_nothing_configured_the_advice_is_to_set_a_host_up() {
        let error = resolve_send_target(&Config::default(), None).unwrap_err();
        assert_eq!(error.exit_code().as_u8(), 21);
        assert!(
            error
                .remedy()
                .is_some_and(|remedy| remedy.command().contains("clift setup")),
            "{error}"
        );
    }

    #[test]
    fn use_moves_the_default_and_nothing_else() {
        let config = use_default(&configured(), "laptop").unwrap();
        assert_eq!(
            config.default_target().map(TargetName::as_str),
            Some("laptop")
        );
        assert_eq!(config.targets().len(), 2);
    }

    fn clift_core_ssh_host(target: &Target) -> String {
        target.ssh_host().to_string()
    }
}
