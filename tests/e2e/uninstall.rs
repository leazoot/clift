//! `clift uninstall`, and the promise that nothing Clift did is permanent.
//!
//! After an uninstall the user's SSH configuration was never touched, their
//! attachments are still on their servers, and Clift's own registrations are
//! gone.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU32, Ordering};

static COUNTER: AtomicU32 = AtomicU32::new(0);

const SSH_CONFIG: &str = "Host example\n    HostName 192.0.2.10\n    User dev\n";

/// A home directory with an SSH configuration in it.
struct Home(PathBuf);

impl Home {
    fn new(label: &str) -> Self {
        let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("clift-uninstall-{label}-{unique}"));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(path.join(".config")).unwrap();
        fs::create_dir_all(path.join(".ssh")).unwrap();
        fs::write(path.join(".ssh").join("config"), SSH_CONFIG).unwrap();
        Self(path)
    }

    fn ssh_config(&self) -> PathBuf {
        self.0.join(".ssh").join("config")
    }

    fn clift_config(&self) -> PathBuf {
        self.0.join(".config").join("clift").join("config.toml")
    }

    /// The hotkey helper's login entry, which lives under this fake `HOME` for
    /// the same reason everything else here does.
    #[cfg(target_os = "macos")]
    fn login_item(&self) -> PathBuf {
        self.0
            .join("Library")
            .join("LaunchAgents")
            .join("dev.clift.hotkey.plist")
    }

    /// Writes a login entry that was never loaded into launchd, which is all
    /// this suite needs: the question is whether `uninstall` finds the file and
    /// takes it away, not whether launchd agrees.
    #[cfg(target_os = "macos")]
    fn register_login_item(&self) {
        let path = self.login_item();
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(
            &path,
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<plist version=\"1.0\"><dict>\n\
             <key>Label</key><string>dev.clift.hotkey</string>\n\
             <key>ProgramArguments</key><array><string>/usr/local/bin/clift</string>\
             <string>hotkey</string></array>\n</dict></plist>\n",
        )
        .unwrap();
    }

    fn run(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_clift"))
            .args(args)
            .env("HOME", &self.0)
            .env("XDG_CONFIG_HOME", self.0.join(".config"))
            .env("NO_COLOR", "1")
            .output()
            .expect("clift must be runnable")
    }
}

impl Drop for Home {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn stderr_of(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

/// Acceptance 1: a dry run lists everything and changes nothing.
#[test]
fn a_dry_run_lists_the_changes_and_makes_none() {
    let home = Home::new("dry");
    home.run(&["target", "add", "core"]);

    let before_config = fs::read(home.clift_config()).unwrap();

    let output = home.run(&["uninstall", "--dry-run"]);
    assert!(output.status.success(), "{}", stderr_of(&output));
    let text = stderr_of(&output);
    assert!(text.contains("Would make these changes:"), "{text}");
    assert!(
        text.contains("--purge"),
        "the configuration is kept, and says so: {text}"
    );
    assert!(text.contains("clift clean core --all"), "{text}");

    assert_eq!(fs::read(home.clift_config()).unwrap(), before_config);
}

/// Acceptance 2, 3 and 5: Clift's own configuration stays and
/// `~/.ssh/config` is untouched.
#[test]
fn uninstalling_leaves_the_users_own_files_exactly_as_they_were() {
    let home = Home::new("default");
    let ssh_before = fs::read(home.ssh_config()).unwrap();
    home.run(&["target", "add", "core"]);

    let output = home.run(&["uninstall"]);
    assert!(output.status.success(), "{}", stderr_of(&output));

    assert!(
        home.clift_config().exists(),
        "the configuration must survive an uninstall without --purge"
    );
    assert_eq!(
        fs::read(home.ssh_config()).unwrap(),
        ssh_before,
        "the SSH configuration was touched"
    );
    let text = stderr_of(&output);
    assert!(text.contains("left alone"), "{text}");
}

/// Acceptance 4: `--purge` removes the local configuration, and still only
/// reports the remote inboxes.
#[test]
fn purge_removes_the_configuration_and_still_asks_about_nothing_remote() {
    let home = Home::new("purge");
    home.run(&["target", "add", "core"]);

    // Without a terminal to answer, the confirmation has to be given up front.
    let refused = home.run(&["uninstall", "--purge"]);
    assert_eq!(refused.status.code(), Some(20), "{}", stderr_of(&refused));
    assert!(
        home.clift_config().exists(),
        "a refused purge deleted the config"
    );

    let output = home.run(&["uninstall", "--purge", "--yes"]);
    assert!(output.status.success(), "{}", stderr_of(&output));
    assert!(
        !home.clift_config().exists(),
        "the configuration survived --purge"
    );
    assert!(
        !home.clift_config().parent().unwrap().exists(),
        "--purge left Clift's own, now empty, configuration directory behind"
    );

    // The remote side is reported, never acted on.
    let text = stderr_of(&output);
    assert!(text.contains("clift clean core --all"), "{text}");
}

/// Uninstalling twice is not an error and changes nothing the second
/// time.
#[test]
fn uninstalling_twice_is_harmless() {
    let home = Home::new("twice");
    home.run(&["target", "add", "core"]);

    assert!(home.run(&["uninstall"]).status.success());
    let after_first = fs::read(home.clift_config()).unwrap();

    let second = home.run(&["uninstall"]);
    assert!(second.status.success(), "{}", stderr_of(&second));
    assert_eq!(fs::read(home.clift_config()).unwrap(), after_first);
}

/// An uninstall on a machine that never had anything set up is not an error.
#[test]
fn uninstalling_a_clean_machine_is_not_an_error() {
    let home = Home::new("clean");
    let output = home.run(&["uninstall"]);
    assert!(output.status.success(), "{}", stderr_of(&output));
    assert!(!home.clift_config().exists());
}

/// The login entry is Clift's own, and it points at the Clift binary. Leaving
/// it behind means the next login starts something the user just removed, so
/// `uninstall` takes it with everything else -- and a dry run still only
/// describes it.
///
/// macOS only, because that is the platform whose entry this suite can put in
/// a fake `HOME`. The Windows entry lives in the Start Menu's Startup folder,
/// which is not under `HOME`, and writing a real one from a test would put a
/// script on the developer's own machine.
#[cfg(target_os = "macos")]
#[test]
fn the_hotkey_login_item_goes_with_the_uninstall() {
    let home = Home::new("login-item");
    home.register_login_item();

    let dry = home.run(&["uninstall", "--dry-run"]);
    assert!(dry.status.success(), "{}", stderr_of(&dry));
    let text = stderr_of(&dry);
    assert!(
        text.contains("dev.clift.hotkey.plist"),
        "the dry run must name the entry it would remove: {text}"
    );
    assert!(
        home.login_item().exists(),
        "a dry run removed the login item"
    );

    let output = home.run(&["uninstall"]);
    assert!(output.status.success(), "{}", stderr_of(&output));
    assert!(
        !home.login_item().exists(),
        "the login item survived the uninstall: {}",
        stderr_of(&output)
    );
    assert!(
        stderr_of(&output).contains("not start at your next login"),
        "the user is told what stopped: {}",
        stderr_of(&output)
    );
}

/// A machine with no helper registered must say so rather than say nothing:
/// a line that only appears sometimes is one a reader cannot rely on.
#[test]
fn an_uninstall_with_no_login_item_says_there_was_none() {
    let home = Home::new("no-login-item");
    let output = home.run(&["uninstall", "--dry-run"]);
    assert!(output.status.success(), "{}", stderr_of(&output));
    assert!(
        stderr_of(&output).contains("no hotkey helper is registered"),
        "{}",
        stderr_of(&output)
    );
}
