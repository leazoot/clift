//! Reading the user's SSH configuration through the system client.
//!
//! The unit tests in `clift-core` read a captured answer. These read a live
//! one: a real `ssh -G` against a real config file, so that a change in the
//! output format shows up here rather than in a user's terminal.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use clift_core::ports::SshConfigSource;
use clift_transport::probe::OpenSshTransport;
use clift_transport::proc::SshRunner;
use std::io::Write;
use std::path::PathBuf;

/// A throwaway config in a private directory. The addresses are RFC 5737
/// documentation addresses, so nothing here can reach a real machine.
fn config_file() -> PathBuf {
    let directory = std::env::temp_dir().join(format!("clift-ssh-config-{}", std::process::id()));
    std::fs::create_dir_all(&directory).expect("the temporary directory must be creatable");
    let path = directory.join("ssh_config");
    let mut file = std::fs::File::create(&path).expect("the config must be writable");
    file.write_all(
        b"Host demo-core\n\
          \x20   HostName 192.0.2.10\n\
          \x20   Port 2222\n\
          \x20   User dev\n\
          \n\
          Host demo-hk\n\
          \x20   HostName 192.0.2.20\n\
          \x20   User dev\n\
          \x20   ProxyJump demo-core\n",
    )
    .expect("the config must be writable");
    path
}

fn transport(config: &PathBuf) -> OpenSshTransport {
    OpenSshTransport::with_runner(SshRunner::new().with_config_file(config))
}

#[test]
fn a_custom_port_and_user_are_read_from_the_users_own_configuration() {
    let config = config_file();
    let settings = transport(&config).settings_for("demo-core").unwrap();

    assert_eq!(settings.alias(), "demo-core");
    assert_eq!(settings.host_name(), "192.0.2.10");
    assert_eq!(settings.port(), 2222);
    assert_eq!(settings.user(), "dev");
    assert_eq!(settings.proxy_jump(), None);
}

#[test]
fn a_jump_host_is_recognised_without_clift_doing_anything_about_it() {
    let config = config_file();
    let settings = transport(&config).settings_for("demo-hk").unwrap();

    assert_eq!(settings.port(), 22, "the default port, not the jump host's");
    assert_eq!(
        settings.proxy_jump(),
        Some("demo-core"),
        "Clift reports the jump; ssh performs it"
    );
    assert!(settings.summary().contains("via demo-core"), "{settings}");
}

/// The live answer contains `identityfile` lines. None of them may
/// survive into anything Clift can print.
#[test]
fn a_live_answer_carries_no_private_key_location() {
    let config = config_file();
    let settings = transport(&config).settings_for("demo-core").unwrap();
    let rendered = format!("{settings} {settings:?} {}", settings.summary());
    for forbidden in ["identityfile", "id_ed25519", "id_rsa", ".ssh/"] {
        assert!(
            !rendered.contains(forbidden),
            "{forbidden} reached a printable value: {rendered}"
        );
    }
}

/// An alias nobody configured resolves to itself, which is what `ssh` would
/// try to connect to. Clift reports that rather than inventing a failure the
/// client would not have had.
#[test]
fn an_unconfigured_alias_resolves_to_itself() {
    let config = config_file();
    let settings = transport(&config)
        .settings_for("not-in-the-config")
        .unwrap();
    assert_eq!(settings.host_name(), "not-in-the-config");
    assert_eq!(settings.port(), 22);
}

/// The port, not the inherent method: `setup` will be handed a
/// `&dyn SshConfigSource`, and that is the path that has to work.
#[test]
fn the_configuration_port_is_implemented_by_the_openssh_transport() {
    let config = config_file();
    let transport = transport(&config);
    let source: &dyn SshConfigSource = &transport;
    assert_eq!(source.settings_for("demo-core").unwrap().port(), 2222);
}
