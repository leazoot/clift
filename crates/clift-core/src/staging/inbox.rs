//! Deciding where the remote inbox goes, and making sure it is private.

use crate::domain::{RemotePath, SafeFileName};
use crate::error::{CliftError, ErrorKind, Stage};
use crate::ports::{RemoteFs, TransportTarget};

/// The inbox, and every directory Clift creates beneath it, is private to the
/// remote account. Nothing about Clift's design assumes the remote host is
/// otherwise trustworthy, so this is the one guarantee it does make.
pub const INBOX_MODE: u32 = 0o700;

/// The value `setup` writes, meaning "wherever Clift would put it anyway".
///
/// Duplicated from the configuration module rather than imported, because the
/// staging layer must not depend on how configuration is stored -- it only
/// needs to recognise the one string that means "no preference".
const DEFAULT_REMOTE_DIR: &str = "~/.cache/clift/inbox";

const CACHE_DIR: &str = ".cache";
const CLIFT_DIR: &str = "clift";
const INBOX_DIR: &str = "inbox";

/// Directories a private inbox must never be placed under.
///
/// All of them are world writable with the sticky bit, which means another
/// account on the host can create entries alongside Clift's and play symlink
/// games with them. The specification rules the default out; this rules it out even when
/// the host asks for it through `XDG_CACHE_HOME`.
const PUBLIC_TEMP_ROOTS: [&str; 3] = ["/tmp", "/var/tmp", "/dev/shm"];

/// Why the inbox ended up where it did.
///
/// Carried out of the resolution rather than logged inside it: `clift-core`
/// does not write to a terminal, and a user whose `XDG_CACHE_HOME` was ignored
/// deserves to be told rather than left to wonder (the specification, explicit over
/// guessing).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InboxRootSource {
    /// The user said where it goes, in `targets.<name>.remote_dir`.
    ///
    /// This wins over everything else, including the host's own
    /// `XDG_CACHE_HOME`: an explicit setting that is quietly overruled is worse
    /// than no setting at all.
    Configured,
    /// The host's own `XDG_CACHE_HOME`.
    CacheHome,
    /// The host nominated nothing, so the home directory was used.
    HomeDefault,
    /// The host nominated somewhere Clift will not use; the string says why.
    CacheHomeRejected(String),
}

/// Where the inbox is, and how that was decided.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InboxLocation {
    root: RemotePath,
    home: RemotePath,
    source: InboxRootSource,
}

impl InboxLocation {
    /// The inbox root. Every batch directory lives under this and nowhere else.
    #[must_use]
    pub fn root(&self) -> &RemotePath {
        &self.root
    }

    /// The remote home directory, worth caching in the config so that later
    /// sends need not ask again.
    #[must_use]
    pub fn home(&self) -> &RemotePath {
        &self.home
    }

    #[must_use]
    pub fn source(&self) -> &InboxRootSource {
        &self.source
    }

    /// What the user should be told about this choice, if anything.
    #[must_use]
    pub fn warning(&self) -> Option<String> {
        match &self.source {
            InboxRootSource::Configured
            | InboxRootSource::CacheHome
            | InboxRootSource::HomeDefault => None,
            InboxRootSource::CacheHomeRejected(reason) => Some(format!(
                "ignored the remote XDG_CACHE_HOME because {reason}; using {} instead",
                self.root
            )),
        }
    }
}

/// The inbox at the location the user configured.
///
/// `~/` is expanded against the remote home, because that is what the user
/// means by it and Clift is the only thing that will ever read the value. A
/// world-writable location is refused outright rather than quietly relocated:
/// the user asked for something specific, and ignoring that is worse than
/// telling them it cannot be done.
fn configured_root(home: &RemotePath, value: &str) -> Result<InboxLocation, CliftError> {
    let expanded = match value.strip_prefix("~/") {
        Some(rest) => format!("{}/{rest}", home.as_str().trim_end_matches('/')),
        None => value.to_string(),
    };

    let root = RemotePath::new(expanded)
        .map_err(|error| error.into_clift(Stage::Staging, ErrorKind::Config))?;

    if let Some(public) = public_temp_root(&root) {
        return Err(CliftError::new(
            Stage::Staging,
            ErrorKind::Config,
            format!(
                "the configured inbox {root} is inside {public}, which every account on the host can write to"
            ),
        )
        .with_remedy(crate::error::Remedy::new(
            "Put it somewhere private instead:",
            "clift config set targets.<name>.remote_dir '~/.cache/clift/inbox'",
        )));
    }

    Ok(InboxLocation {
        root,
        home: home.clone(),
        source: InboxRootSource::Configured,
    })
}

/// Works out where the inbox belongs, without creating anything.
///
/// # Errors
/// Fails when the host cannot be reached, or when a path it reports cannot be
/// extended into an inbox path.
pub fn locate_inbox(
    remote: &dyn RemoteFs,
    target: &TransportTarget,
    configured: Option<&str>,
) -> Result<InboxLocation, CliftError> {
    let home = remote.resolve_home(target)?;
    // Only asked for when it can still change the answer. A configured
    // location wins outright, and the round trip that asks the host about its
    // cache directory costs seconds on a real connection.
    let cache_home = match expandable(configured) {
        Some(_) => None,
        None => remote.resolve_cache_home(target)?,
    };
    resolve(home, cache_home, configured)
}

/// Works out where the inbox belongs and makes sure it exists with mode 0700.
///
/// # Errors
/// Fails when the host cannot be reached, and when the inbox exists with
/// different permissions -- which is reported rather than corrected, because
/// changing the permissions of a directory the user already has would hide the
/// problem instead of showing it (exit code 25).
pub fn ensure_inbox(
    remote: &dyn RemoteFs,
    target: &TransportTarget,
    configured: Option<&str>,
) -> Result<InboxLocation, CliftError> {
    let location = locate_inbox(remote, target, configured)?;
    remote.ensure_dir(target, location.root(), INBOX_MODE)?;
    Ok(location)
}

/// The configured location, when it is one that changes anything.
///
/// The default string is what every configuration written by `setup` contains,
/// and it means "wherever Clift would have put it anyway" -- so it is not
/// treated as an instruction to override the host's own cache directory.
fn expandable(configured: Option<&str>) -> Option<&str> {
    let value = configured?.trim();
    if value.is_empty() || value == DEFAULT_REMOTE_DIR {
        return None;
    }
    Some(value)
}

/// The rule itself, separated from the round trips so it can be tested exactly.
fn resolve(
    home: RemotePath,
    cache_home: Option<RemotePath>,
    configured: Option<&str>,
) -> Result<InboxLocation, CliftError> {
    if let Some(value) = expandable(configured) {
        return configured_root(&home, value);
    }
    let (base, source) = match cache_home {
        Some(cache) => match public_temp_root(&cache) {
            None => (cache, InboxRootSource::CacheHome),
            Some(root) => (
                home.join(&component(CACHE_DIR)?),
                InboxRootSource::CacheHomeRejected(format!(
                    "{cache} is inside {root}, which every account on the host can write to"
                )),
            ),
        },
        None => (
            home.join(&component(CACHE_DIR)?),
            InboxRootSource::HomeDefault,
        ),
    };

    let root = base
        .join(&component(CLIFT_DIR)?)
        .join(&component(INBOX_DIR)?);
    Ok(InboxLocation { root, home, source })
}

/// The public temporary directory `path` sits in, if it sits in one.
fn public_temp_root(path: &RemotePath) -> Option<&'static str> {
    PUBLIC_TEMP_ROOTS
        .into_iter()
        .find(|root| RemotePath::new(*root).is_ok_and(|public| path.is_within(&public)))
}

fn component(name: &'static str) -> Result<SafeFileName, CliftError> {
    SafeFileName::new(name).map_err(|error| error.into_clift(Stage::Staging, ErrorKind::Internal))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::RecordingTransport;

    fn path(value: &str) -> RemotePath {
        RemotePath::new(value).unwrap_or_else(|error| panic!("bad test path {value:?}: {error}"))
    }

    #[test]
    fn without_a_cache_home_the_inbox_sits_under_the_home_directory() {
        let location = resolve(path("/home/dev"), None, None).unwrap();
        assert_eq!(location.root().as_str(), "/home/dev/.cache/clift/inbox");
        assert_eq!(*location.source(), InboxRootSource::HomeDefault);
        assert_eq!(location.warning(), None);
    }

    #[test]
    fn a_cache_home_the_host_nominates_is_respected() {
        let location = resolve(path("/home/dev"), Some(path("/data/cache")), None).unwrap();
        assert_eq!(location.root().as_str(), "/data/cache/clift/inbox");
        assert_eq!(*location.source(), InboxRootSource::CacheHome);
        assert_eq!(location.warning(), None);
    }

    /// A private inbox may not live where every account on the host
    /// can create entries next to it.
    #[test]
    fn a_cache_home_in_a_public_temporary_directory_is_refused_and_reported() {
        for public in ["/tmp", "/tmp/cache", "/var/tmp/x", "/dev/shm/y"] {
            let location = resolve(path("/home/dev"), Some(path(public)), None).unwrap();
            assert_eq!(
                location.root().as_str(),
                "/home/dev/.cache/clift/inbox",
                "{public} should have been refused"
            );
            let warning = location.warning().unwrap_or_default();
            assert!(warning.contains("XDG_CACHE_HOME"), "{warning}");
            assert!(warning.contains("write"), "{warning}");
        }
    }

    #[test]
    fn a_directory_merely_named_like_a_temporary_one_is_fine() {
        // `/tmpfiles` is not inside `/tmp`; prefix matching on strings alone
        // would get this wrong.
        let location = resolve(path("/home/dev"), Some(path("/tmpfiles")), None).unwrap();
        assert_eq!(location.root().as_str(), "/tmpfiles/clift/inbox");
        assert_eq!(*location.source(), InboxRootSource::CacheHome);
    }

    #[test]
    fn the_default_root_is_never_a_public_temporary_directory() {
        // Whatever the home directory is, the default must not land in /tmp.
        for home in ["/home/dev", "/root", "/Users/someone", "/"] {
            let location = resolve(path(home), None, None).unwrap();
            for public in PUBLIC_TEMP_ROOTS {
                assert!(
                    !location.root().is_within(&path(public)),
                    "{home} produced {}",
                    location.root()
                );
            }
        }
    }

    /// The configured location wins, and `~/` means the remote home.
    #[test]
    fn a_configured_inbox_is_used_and_its_tilde_expanded() {
        let location = resolve(
            path("/home/dev"),
            Some(path("/data/cache")),
            Some("~/attachments"),
        )
        .unwrap();
        assert_eq!(location.root().as_str(), "/home/dev/attachments");
        assert_eq!(location.source(), &InboxRootSource::Configured);
        assert_eq!(location.warning(), None);

        let absolute = resolve(path("/home/dev"), None, Some("/srv/clift")).unwrap();
        assert_eq!(absolute.root().as_str(), "/srv/clift");
    }

    /// The string `setup` writes means "wherever Clift would have put it", not
    /// "override the host's cache directory".
    #[test]
    fn the_default_string_is_not_treated_as_an_override() {
        let location = resolve(
            path("/home/dev"),
            Some(path("/data/cache")),
            Some("~/.cache/clift/inbox"),
        )
        .unwrap();
        assert_eq!(location.source(), &InboxRootSource::CacheHome);
        assert_eq!(location.root().as_str(), "/data/cache/clift/inbox");
    }

    /// An explicit setting pointing somewhere world-writable is refused rather
    /// than quietly relocated: the user asked for something specific.
    #[test]
    fn a_configured_public_directory_is_refused_rather_than_ignored() {
        for public in ["/tmp/clift", "/var/tmp/inbox", "/dev/shm/x"] {
            let error = resolve(path("/home/dev"), None, Some(public))
                .expect_err("a world-writable inbox is not usable");
            assert_eq!(error.exit_code().as_u8(), 20, "{public}");
            assert!(error.to_string().contains("every account"), "{public}");
        }
    }

    /// Asking the host about its cache directory costs a round trip, and a
    /// configured location makes the answer irrelevant.
    #[test]
    fn a_configured_inbox_does_not_ask_the_host_about_its_cache_directory() {
        let remote = RecordingTransport::new("/home/dev");
        let target = TransportTarget::new("core");
        remote.advertise_cache_home("/data/cache");

        let location = locate_inbox(&remote, &target, Some("~/attachments")).unwrap();
        assert_eq!(location.root().as_str(), "/home/dev/attachments");
        assert!(
            !remote
                .calls()
                .iter()
                .any(|call| matches!(call, crate::testing::TransportCall::ResolveCacheHome { .. })),
            "a question whose answer cannot matter should not be asked: {:?}",
            remote.calls()
        );
    }

    #[test]
    fn ensuring_the_inbox_asks_for_mode_0700() {
        let remote = RecordingTransport::new("/home/dev");
        let target = TransportTarget::new("core");
        let location = ensure_inbox(&remote, &target, None).unwrap();

        assert_eq!(location.root().as_str(), "/home/dev/.cache/clift/inbox");
        let requested: Vec<(String, u32)> = remote
            .calls()
            .into_iter()
            .filter_map(|call| match call {
                crate::testing::TransportCall::EnsureDir { path, mode } => Some((path, mode)),
                _ => None,
            })
            .collect();
        assert_eq!(
            requested,
            vec![("/home/dev/.cache/clift/inbox".to_string(), 0o700)]
        );
    }

    #[test]
    fn a_host_that_advertises_a_cache_home_is_asked_only_once() {
        let remote = RecordingTransport::new("/home/dev");
        remote.advertise_cache_home("/data/cache");
        let location = ensure_inbox(&remote, &TransportTarget::new("core"), None).unwrap();
        assert_eq!(location.root().as_str(), "/data/cache/clift/inbox");

        // Each round trip costs seconds on a real host (3.6 to 8.4 s
        // on a distant host), so the count is part of the contract.
        let lookups = remote
            .calls()
            .into_iter()
            .filter(|call| {
                matches!(
                    call,
                    crate::testing::TransportCall::ResolveHome { .. }
                        | crate::testing::TransportCall::ResolveCacheHome { .. }
                )
            })
            .count();
        assert_eq!(lookups, 2, "resolution must not re-ask the host");
    }
}
