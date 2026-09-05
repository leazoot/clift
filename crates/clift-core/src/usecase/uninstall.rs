//! Taking Clift back out.
//!
//! The specification, and the promise behind it: nothing Clift did is irreversible. Two
//! kinds of thing are left behind by an installation, and they are treated
//! differently on purpose:
//!
//! - **Clift's own configuration**: kept unless `--purge`, because a user who
//!   is reinstalling should not have to set their hosts up again, and told
//!   about either way;
//! - **the attachments on remote hosts**: never removed as a side effect.
//!   They are the user's files on the user's servers, and "I am uninstalling a
//!   local tool" is not consent to delete them. Their locations are reported so
//!   that removing them is one command away.

use crate::config::Config;
use std::path::{Path, PathBuf};

/// One target's inbox, so the user can find it after Clift is gone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteLeftovers {
    pub name: String,
    pub ssh_host: String,
    pub remote_dir: String,
}

/// Everything an uninstall would do to this machine's own files.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UninstallPlan {
    config_path: PathBuf,
    config_exists: bool,
    remove_config: bool,
    leftovers: Vec<RemoteLeftovers>,
}

impl UninstallPlan {
    #[must_use]
    pub fn config_path(&self) -> &Path {
        &self.config_path
    }

    #[must_use]
    pub const fn config_exists(&self) -> bool {
        self.config_exists
    }

    /// True only with `--purge`.
    #[must_use]
    pub const fn removes_config(&self) -> bool {
        self.remove_config
    }

    /// The inboxes that will still be there afterwards.
    #[must_use]
    pub fn leftovers(&self) -> &[RemoteLeftovers] {
        &self.leftovers
    }
}

/// Works out what uninstalling would leave and what it would take.
#[must_use]
pub fn plan_uninstall(config: &Config, config_path: &Path, purge: bool) -> UninstallPlan {
    UninstallPlan {
        config_path: config_path.to_path_buf(),
        config_exists: config_path.exists(),
        remove_config: purge,
        leftovers: config
            .targets()
            .iter()
            .map(|(name, target)| RemoteLeftovers {
                name: name.as_str().to_string(),
                ssh_host: target.ssh_host().to_string(),
                remote_dir: target.remote_dir().to_string(),
            })
            .collect(),
    }
}

/// The command that would remove one target's attachments.
///
/// Offered rather than run: the specification puts the remote files outside what an
/// uninstall may decide on its own.
#[must_use]
pub fn cleanup_command(leftovers: &RemoteLeftovers) -> String {
    format!("clift clean {} --all --yes", leftovers.name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::usecase::add;

    #[test]
    fn the_default_keeps_the_configuration_and_reports_every_inbox() {
        let config = add(&Config::default(), "core", None).unwrap();
        let config = add(&config, "laptop", Some("my-laptop")).unwrap();

        let plan = plan_uninstall(
            &config,
            Path::new("/home/dev/.config/clift/config.toml"),
            false,
        );
        assert!(!plan.removes_config());
        assert_eq!(plan.leftovers().len(), 2);
        assert_eq!(plan.leftovers()[0].name, "core");
        assert_eq!(plan.leftovers()[1].ssh_host, "my-laptop");
        assert!(
            plan.leftovers()[0].remote_dir.contains("inbox"),
            "the location is what makes this actionable"
        );
    }

    #[test]
    fn purge_removes_the_configuration_and_still_only_reports_the_inboxes() {
        let config = add(&Config::default(), "core", None).unwrap();
        let plan = plan_uninstall(
            &config,
            Path::new("/home/dev/.config/clift/config.toml"),
            true,
        );
        assert!(plan.removes_config());
        assert_eq!(
            plan.leftovers().len(),
            1,
            "purge is about the local configuration, not somebody's server"
        );
    }

    /// The offer is a command the user can run, not a promise Clift kept.
    #[test]
    fn each_inbox_comes_with_the_command_that_would_clear_it() {
        let config = add(&Config::default(), "core", None).unwrap();
        let plan = plan_uninstall(&config, Path::new("/x"), false);
        assert_eq!(
            cleanup_command(&plan.leftovers()[0]),
            "clift clean core --all --yes"
        );
    }

    #[test]
    fn nothing_configured_means_nothing_left_behind() {
        let plan = plan_uninstall(&Config::default(), Path::new("/x"), false);
        assert!(plan.leftovers().is_empty());
    }
}
