//! What the user's SSH configuration says about a host.
//!
//! Clift does not parse `~/.ssh/config` itself. `Host` patterns, `Match`
//! blocks, `Include` files, canonicalisation and the precedence rules between
//! them are a language, and a second implementation of that language would
//! eventually disagree with the first -- at which point Clift would be showing
//! the user a host it is not actually going to connect to. So the system client
//! is asked instead (`ssh -G`), and this module reads its answer.
//!
//! Only four settings are read: the host name, the port, the user and whether
//! a jump host is involved. Everything else in that answer is deliberately
//! dropped, `identityfile` above all: Clift never needs to know where a private
//! key lives, and a field that does not exist cannot be printed by mistake
//!.

use crate::error::{CliftError, ErrorKind, Stage};
use std::fmt;

/// The effective settings for one SSH alias, as the system client resolved them.
///
/// The fields are private and there is no constructor from parts: the only way
/// to obtain one is to parse what `ssh` said, which is what keeps this from
/// drifting into Clift's own opinion about the host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SshHostSettings {
    alias: String,
    host_name: String,
    port: u16,
    user: String,
    proxy_jump: Option<String>,
}

impl SshHostSettings {
    /// The name the user typed, not the name it resolved to.
    #[must_use]
    pub fn alias(&self) -> &str {
        &self.alias
    }

    #[must_use]
    pub fn host_name(&self) -> &str {
        &self.host_name
    }

    #[must_use]
    pub const fn port(&self) -> u16 {
        self.port
    }

    #[must_use]
    pub fn user(&self) -> &str {
        &self.user
    }

    /// The jump host, when the connection goes through one.
    ///
    /// Clift does nothing with this beyond telling the user about it: the jump
    /// is performed by `ssh`, which already knows how.
    #[must_use]
    pub fn proxy_jump(&self) -> Option<&str> {
        self.proxy_jump.as_deref()
    }

    /// The one line `setup` shows before asking for confirmation.
    #[must_use]
    pub fn summary(&self) -> String {
        let mut summary = format!("{}@{}:{}", self.user, self.host_name, self.port);
        if let Some(jump) = &self.proxy_jump {
            summary.push_str(&format!(" via {jump}"));
        }
        summary
    }
}

impl fmt::Display for SshHostSettings {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} ({})", self.alias, self.summary())
    }
}

/// Reads the effective configuration `ssh -G <alias>` printed.
///
/// The format is one lowercase keyword per line followed by its value. Unknown
/// keywords are ignored rather than rejected: the list grows with every OpenSSH
/// release, and failing on a keyword Clift has not heard of would break the tool
/// on an upgrade that has nothing to do with it.
///
/// # Errors
/// Fails when the host name, port or user is missing or unusable. Those three
/// are always present in a well-formed answer, so their absence means the
/// output is not what this function was given to read.
pub fn parse_effective_config(alias: &str, output: &str) -> Result<SshHostSettings, CliftError> {
    let mut host_name = None;
    let mut port = None;
    let mut user = None;
    let mut proxy_jump = None;

    for line in output.lines() {
        let Some((keyword, value)) = line.trim().split_once(char::is_whitespace) else {
            continue;
        };
        let value = value.trim();
        if value.is_empty() {
            continue;
        }
        match keyword.to_ascii_lowercase().as_str() {
            "hostname" => host_name = Some(value.to_string()),
            "port" => port = value.parse::<u16>().ok(),
            "user" => user = Some(value.to_string()),
            // OpenSSH prints this line only when a jump host is configured, and
            // spells "no jump host" as the literal `none`.
            "proxyjump" if !value.eq_ignore_ascii_case("none") => {
                proxy_jump = Some(value.to_string());
            }
            _ => {}
        }
    }

    Ok(SshHostSettings {
        alias: alias.to_string(),
        host_name: require(alias, "hostname", host_name)?,
        port: port.ok_or_else(|| missing(alias, "port"))?,
        user: require(alias, "user", user)?,
        proxy_jump,
    })
}

/// Whether the user's own SSH configuration already multiplexes this host.
///
/// The specification allows Clift to reuse a connection and forbids it from overriding a
/// `ControlMaster` the user set up themselves. Command line options beat the
/// configuration file in OpenSSH, so "do not override" has to mean "do not
/// pass the options at all" -- which means asking first, and this is the
/// question.
///
/// Read from what `ssh -G` printed rather than from `~/.ssh/config`, for the
/// reason this whole module exists: `Match` blocks and `Include` files make
/// the file and the effective settings two different things, and the one that
/// matters is the one the client resolved.
///
/// The two spellings are not symmetric, and both come from real output:
/// `controlmaster` is always printed (`false` when unset), while `controlpath`
/// appears **only** when it is set. See `tests/fixtures/ssh-config/README.md`.
#[must_use]
pub fn multiplexes_already(output: &str) -> bool {
    let path_is_set = effective_setting(output, "controlpath")
        .is_some_and(|value| !value.eq_ignore_ascii_case("none"));
    // `no` and `false` are the same answer; OpenSSH prints the second and
    // accepts the first, and a configuration that survives a release which
    // swaps them is worth the extra word here.
    let master_is_set = effective_setting(output, "controlmaster").is_some_and(|value| {
        !(value.eq_ignore_ascii_case("false")
            || value.eq_ignore_ascii_case("no")
            || value.eq_ignore_ascii_case("none"))
    });
    path_is_set || master_is_set
}

/// The value `ssh -G` printed for one keyword, if it printed one.
///
/// Keywords are lowercase in that output and values may contain spaces, so the
/// split is on the first run of whitespace and nothing else is interpreted.
#[must_use]
pub fn effective_setting<'a>(output: &'a str, keyword: &str) -> Option<&'a str> {
    output.lines().find_map(|line| {
        let (found, value) = line.trim().split_once(char::is_whitespace)?;
        if found.eq_ignore_ascii_case(keyword) {
            let value = value.trim();
            (!value.is_empty()).then_some(value)
        } else {
            None
        }
    })
}

fn require(alias: &str, field: &str, value: Option<String>) -> Result<String, CliftError> {
    value.ok_or_else(|| missing(alias, field))
}

fn missing(alias: &str, field: &str) -> CliftError {
    CliftError::new(
        Stage::Config,
        ErrorKind::Config,
        format!("ssh did not report a {field} for {alias}"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Trimmed from a real `ssh -G` run; see tests/fixtures/ssh-config/README.md.
    const REAL_OUTPUT: &str = include_str!("../../../../tests/fixtures/ssh-config/effective.txt");

    #[test]
    fn the_four_settings_that_matter_are_read_from_a_real_answer() {
        let settings = parse_effective_config("demo-core", REAL_OUTPUT).unwrap();
        assert_eq!(settings.alias(), "demo-core");
        assert_eq!(settings.host_name(), "192.0.2.10");
        assert_eq!(settings.port(), 2222);
        assert_eq!(settings.user(), "dev");
        assert_eq!(settings.proxy_jump(), None);
        assert_eq!(settings.summary(), "dev@192.0.2.10:2222");
    }

    /// No field exists to hold a key path, so none can be printed.
    #[test]
    fn nothing_about_a_private_key_survives_the_parse() {
        assert!(
            REAL_OUTPUT.contains("identityfile"),
            "the fixture must contain the lines this test is about"
        );
        let settings = parse_effective_config("demo-core", REAL_OUTPUT).unwrap();
        let rendered = format!("{settings} {settings:?} {}", settings.summary());
        for forbidden in ["identityfile", "id_ed25519", "id_rsa", ".ssh/"] {
            assert!(
                !rendered.contains(forbidden),
                "{forbidden} reached a value Clift can print: {rendered}"
            );
        }
    }

    /// Both fixtures are real `ssh -G` output; the difference between them is
    /// exactly the three options Clift would add.
    #[test]
    fn the_users_own_multiplexing_is_recognised_and_left_alone() {
        const MULTIPLEXED: &str =
            include_str!("../../../../tests/fixtures/ssh-config/multiplexed.txt");

        assert!(
            !multiplexes_already(REAL_OUTPUT),
            "an unconfigured host does not multiplex, so Clift may offer to"
        );
        assert!(
            multiplexes_already(MULTIPLEXED),
            "a host the user already multiplexes must be left exactly as it is"
        );
    }

    /// The half of the answer that is easy to get wrong: `controlmaster` is
    /// printed either way, so its mere presence proves nothing.
    #[test]
    fn a_printed_but_unset_controlmaster_is_not_multiplexing() {
        assert!(REAL_OUTPUT.contains("controlmaster false"));
        for spelling in [
            "controlmaster false",
            "controlmaster no",
            "controlpath none",
        ] {
            assert!(
                !multiplexes_already(&format!("user dev\n{spelling}\n")),
                "{spelling} means the user has not set anything up"
            );
        }
        for spelling in [
            "controlmaster auto",
            "controlmaster yes",
            "controlpath /tmp/s",
        ] {
            assert!(
                multiplexes_already(&format!("user dev\n{spelling}\n")),
                "{spelling}"
            );
        }
    }

    #[test]
    fn a_keyword_that_was_never_printed_has_no_value() {
        assert_eq!(effective_setting(REAL_OUTPUT, "controlpath"), None);
        assert_eq!(effective_setting(REAL_OUTPUT, "port"), Some("2222"));
        // A keyword printed with no value is the same as one not printed.
        assert_eq!(effective_setting("controlpath  \n", "controlpath"), None);
    }

    #[test]
    fn a_jump_host_is_noticed_and_named() {
        let output = "user dev\nhostname 192.0.2.20\nport 22\nproxyjump demo-core\n";
        let settings = parse_effective_config("demo-hk", output).unwrap();
        assert_eq!(settings.proxy_jump(), Some("demo-core"));
        assert_eq!(settings.summary(), "dev@192.0.2.20:22 via demo-core");
    }

    /// OpenSSH spells "no jump host" as a value, not as an absent line.
    #[test]
    fn an_explicit_none_is_not_a_jump_host() {
        let output = "user dev\nhostname h\nport 22\nproxyjump none\n";
        assert_eq!(
            parse_effective_config("x", output).unwrap().proxy_jump(),
            None
        );
    }

    #[test]
    fn a_keyword_clift_has_never_heard_of_is_ignored_rather_than_fatal() {
        let output = "user dev\nhostname h\nport 22\nsomethingnewin2030 yes\n";
        assert!(parse_effective_config("x", output).is_ok());
    }

    #[test]
    fn an_answer_missing_a_required_setting_is_refused() {
        for output in [
            "hostname h\nport 22\n",
            "user dev\nport 22\n",
            "user dev\nhostname h\n",
            "user dev\nhostname h\nport not-a-number\n",
            "",
        ] {
            let error = parse_effective_config("x", output)
                .expect_err("a well-formed answer always has all three");
            assert_eq!(error.exit_code().as_u8(), 20, "{output:?}");
        }
    }

    #[test]
    fn a_port_outside_the_range_is_refused_rather_than_wrapped() {
        assert!(parse_effective_config("x", "user d\nhostname h\nport 65536\n").is_err());
    }
}
