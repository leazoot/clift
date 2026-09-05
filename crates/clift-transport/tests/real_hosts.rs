//! The transport layer against the project's real hosts.
//!
//! The specification forbids claiming support for a host that has not actually been
//! reached, so `core` (a non-root account on port 2222) and `hk` (reached
//! through `ProxyJump core`) are exercised here rather than described.
//!
//! Read-only apart from Clift's own inbox, and the probe directory it creates
//! is removed again. Destructive scenarios belong in the containers.
//!
//! These hosts exist only on the maintainer's machine, so the test names them
//! through `CLIFT_REAL_HOSTS` and says loudly when it is skipped:
//!
//! ```text
//! CLIFT_REAL_HOSTS=core,hk cargo test -p clift-transport --test real_hosts
//! ```

#![allow(clippy::unwrap_used)]

#[path = "../../../tests/e2e/fixtures.rs"]
mod fixtures;

use clift_core::domain::RemotePath;
use clift_core::ports::{CheckStatus, TransportTarget};
use clift_transport::probe::OpenSshTransport;
use std::io::Write as _;

/// Straight to the file descriptor, so the evidence is visible without
/// `--nocapture` and a skip cannot be mistaken for a pass.
fn announce(line: &str) {
    let mut stderr = std::io::stderr();
    let _ = writeln!(stderr, "{line}");
    let _ = stderr.flush();
}

fn hosts() -> Vec<String> {
    std::env::var("CLIFT_REAL_HOSTS")
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|host| !host.is_empty())
        .map(str::to_string)
        .collect()
}

#[test]
fn the_named_real_hosts_probe_cleanly_and_accept_a_private_inbox() {
    let hosts = hosts();
    if hosts.is_empty() {
        announce(
            "SKIPPED the_named_real_hosts_probe_cleanly_and_accept_a_private_inbox: \
             CLIFT_REAL_HOSTS is not set; this test proved nothing",
        );
        return;
    }

    // The user's own ~/.ssh/config, with no options added: the point is to
    // prove Clift works with the configuration the user already has.
    let transport = OpenSshTransport::new();

    for host in hosts {
        let target = TransportTarget::new(&host);
        announce(&format!("--- {host} ---"));

        let report = transport.probe(&target).unwrap();
        for check in &report.checks {
            announce(&format!(
                "  {:<16} {:?}  {}",
                check.name, check.status, check.detail
            ));
            assert_ne!(
                check.status,
                CheckStatus::Fail,
                "{host}: {} failed: {}",
                check.name,
                check.detail
            );
        }

        let home = transport.resolve_home(&target).unwrap();
        announce(&format!("  home             {home}"));

        let inbox = RemotePath::new(format!("{home}/.cache/clift/inbox")).unwrap();
        transport.ensure_dir(&target, &inbox, 0o700).unwrap();
        let entry = transport.stat(&target, &inbox).unwrap().unwrap();
        assert_eq!(entry.mode, Some(0o700), "{host}: the inbox must be private");
        announce(&format!("  inbox            {inbox} mode 0700"));

        // A directory of its own, created and then taken away again: the real
        // hosts are not a scratch pad.
        let probe_dir = RemotePath::new(format!("{inbox}/clift-verify-task-019")).unwrap();
        transport.ensure_dir(&target, &probe_dir, 0o700).unwrap();
        assert_eq!(
            transport.stat(&target, &probe_dir).unwrap().unwrap().mode,
            Some(0o700)
        );
        transport.remove(&target, &probe_dir).unwrap();
        assert!(
            transport.stat(&target, &probe_dir).unwrap().is_none(),
            "{host}: the probe directory was left behind"
        );
        announce("  verify dir       created 0700, then removed");
    }
}

/// Two acceptance checks against the hosts they name: `core` must come
/// back on port 2222 as `dev`, and `hk` must be recognised as going through a
/// jump host.
///
/// The answers are read live and never committed -- they contain the
/// maintainer's own paths. What is asserted here is the shape, and that no key
/// location survives into anything printable.
#[test]
fn the_real_hosts_resolve_to_the_settings_they_are_documented_to_have() {
    let hosts = hosts();
    if hosts.is_empty() {
        announce(
            "SKIPPED the_real_hosts_resolve_to_the_settings_they_are_documented_to_have: \
             CLIFT_REAL_HOSTS is not set; this test proved nothing",
        );
        return;
    }

    let transport = OpenSshTransport::new();
    for host in hosts {
        let settings = transport
            .settings_for(&host)
            .unwrap_or_else(|error| panic!("{host}: {error}"));
        assert_eq!(settings.alias(), host);
        assert!(!settings.user().is_empty(), "{host} resolved to no user");
        assert!(settings.port() > 0, "{host} resolved to no port");

        match host.as_str() {
            "core" => {
                assert_eq!(settings.port(), 2222, "core is documented as port 2222");
                assert_eq!(settings.user(), "dev");
                assert_eq!(settings.proxy_jump(), None);
            }
            "hk" => {
                assert_eq!(
                    settings.proxy_jump(),
                    Some("core"),
                    "hk is documented as reached through ProxyJump core"
                );
            }
            _ => {}
        }

        let rendered = format!("{settings} {settings:?}");
        for forbidden in ["identityfile", "id_ed25519", "id_rsa", ".ssh/"] {
            assert!(
                !rendered.contains(forbidden),
                "{forbidden} reached a printable value for {host}"
            );
        }
        announce(&format!("real host {host}: {}", settings.summary()));
    }
}
