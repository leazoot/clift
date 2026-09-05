//! Where Clift's own files live on this machine.
//!
//! Three places, one policy: the config file, the cache (this host's inbox)
//! and the private run directory. On Unix they follow the XDG base directory
//! specification with `HOME` as the fallback; on Windows they follow the
//! per-user `APPDATA` (roaming, for the config) and `LOCALAPPDATA` (for what
//! should stay on this machine) directories, with `USERPROFILE` as the
//! fallback. Nothing here guesses: a machine that provides none of the
//! variables gets an error naming them and a command for its own shell.
//!
//! The resolvers take the environment as a function so that both platforms'
//! rules are exercised by tests on whichever host runs them.

use crate::error::{CliftError, ErrorKind, Remedy, Stage};
use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// The two families of file layout Clift knows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Platform {
    Unix,
    Windows,
}

impl Platform {
    /// The platform this binary was built for.
    #[must_use]
    pub const fn current() -> Self {
        if cfg!(windows) {
            Self::Windows
        } else {
            Self::Unix
        }
    }
}

/// An environment lookup: the variable's value, or `None` when it is unset or
/// empty. An empty value is treated as unset because that is what every shell
/// convention around these variables does.
pub type Lookup<'a> = &'a dyn Fn(&str) -> Option<OsString>;

/// The process environment, as a [`Lookup`].
#[must_use]
pub fn process_environment(name: &str) -> Option<OsString> {
    std::env::var_os(name).filter(|value| !value.is_empty())
}

/// The name Clift's directory carries on each platform. Lower case under XDG
/// directories, where everything is; capitalised under `AppData`, where
/// everything is.
const fn directory_name(platform: Platform) -> &'static str {
    match platform {
        Platform::Unix => "clift",
        Platform::Windows => "Clift",
    }
}

/// Directories that are writable by everyone, and so cannot hold anything
/// private. Refused as a base even when the environment nominates one.
const PUBLIC_ROOTS: [&str; 3] = ["/tmp", "/var/tmp", "/dev/shm"];

/// True when `path` is one of the world-writable roots, or sits inside one.
#[must_use]
pub fn is_public(path: &Path) -> bool {
    PUBLIC_ROOTS
        .iter()
        .any(|root| path == Path::new(root) || path.starts_with(root))
}

/// Why a place could not be resolved. Converted into a [`CliftError`] by the
/// caller, which knows which stage it was working for.
#[derive(Debug)]
pub struct Unlocated {
    /// The variables that were consulted, in order, none of which was usable.
    consulted: &'static [&'static str],
    remedy: Remedy,
}

impl Unlocated {
    /// Builds the error for a caller's stage and kind, saying what it was
    /// looking for.
    #[must_use]
    pub fn into_error(self, stage: Stage, kind: ErrorKind, looking_for: &str) -> CliftError {
        CliftError::new(
            stage,
            kind,
            format!(
                "cannot locate {looking_for}: none of {} is set",
                self.consulted.join(", ")
            ),
        )
        .with_remedy(self.remedy)
    }
}

/// The path of `config.toml`.
///
/// Unix: `$XDG_CONFIG_HOME/clift/config.toml`, else `~/.config/clift/config.toml`.
/// Windows: `%APPDATA%\Clift\config.toml`, else `%USERPROFILE%\AppData\Roaming\Clift\config.toml`.
///
/// # Errors
/// Fails when none of the variables yields a base directory.
pub fn config_file(platform: Platform, env: Lookup<'_>) -> Result<PathBuf, Unlocated> {
    let base = match platform {
        Platform::Unix => match env("XDG_CONFIG_HOME") {
            Some(xdg) => PathBuf::from(xdg),
            None => match env("HOME") {
                Some(home) => PathBuf::from(home).join(".config"),
                None => {
                    return Err(Unlocated {
                        consulted: &["XDG_CONFIG_HOME", "HOME"],
                        remedy: Remedy::new(
                            "Set one of them, for example:",
                            "export XDG_CONFIG_HOME=\"$HOME/.config\"",
                        ),
                    });
                }
            },
        },
        Platform::Windows => match env("APPDATA").filter(is_absolute_windows) {
            Some(appdata) => PathBuf::from(appdata),
            None => match env("USERPROFILE").filter(is_absolute_windows) {
                Some(profile) => PathBuf::from(profile).join("AppData").join("Roaming"),
                None => {
                    return Err(Unlocated {
                        consulted: &["APPDATA", "USERPROFILE"],
                        remedy: Remedy::new(
                            "Windows sets both for every account; in PowerShell, restore the one Clift needs:",
                            "$env:APPDATA = \"$env:USERPROFILE\\AppData\\Roaming\"",
                        ),
                    });
                }
            },
        },
    };
    Ok(base.join(directory_name(platform)).join("config.toml"))
}

/// Clift's cache directory: where this host's inbox lives.
///
/// Unix: `$XDG_CACHE_HOME/clift`, else `~/.cache/clift`; a nomination inside a
/// public directory, or a relative one, is ignored rather than followed.
/// Windows: `%LOCALAPPDATA%\Clift`, else `%USERPROFILE%\AppData\Local\Clift`.
///
/// # Errors
/// Fails when none of the variables yields a usable base directory.
pub fn cache_dir(platform: Platform, env: Lookup<'_>) -> Result<PathBuf, Unlocated> {
    let base = match platform {
        Platform::Unix => match usable_unix_base(env, &["XDG_CACHE_HOME"]) {
            Some(cache) => cache,
            None => unix_home_subdirectory(env, ".cache", &["XDG_CACHE_HOME", "HOME"])?,
        },
        Platform::Windows => windows_local_base(env)?,
    };
    Ok(base.join(directory_name(platform)))
}

/// Clift's private run directory: scratch files, session leases, control
/// sockets. Preferred over the cache because a runtime directory is what it is
/// for, and it is cleared at logout.
///
/// Unix: `$XDG_RUNTIME_DIR/clift`, else `$XDG_CACHE_HOME/clift`, else
/// `~/.cache/clift`, with the same refusals as [`cache_dir`].
/// Windows: the same directory as [`cache_dir`].
///
/// # Errors
/// Fails when none of the variables yields a usable base directory.
pub fn run_dir(platform: Platform, env: Lookup<'_>) -> Result<PathBuf, Unlocated> {
    let base = match platform {
        Platform::Unix => match usable_unix_base(env, &["XDG_RUNTIME_DIR", "XDG_CACHE_HOME"]) {
            Some(runtime) => runtime,
            None => unix_home_subdirectory(
                env,
                ".cache",
                &["XDG_RUNTIME_DIR", "XDG_CACHE_HOME", "HOME"],
            )?,
        },
        Platform::Windows => windows_local_base(env)?,
    };
    Ok(base.join(directory_name(platform)))
}

/// The first of `variables` that names an absolute, non-public directory.
fn usable_unix_base(env: Lookup<'_>, variables: &[&str]) -> Option<PathBuf> {
    variables.iter().find_map(|variable| {
        let candidate = PathBuf::from(env(variable)?);
        (candidate.is_absolute() && !is_public(&candidate)).then_some(candidate)
    })
}

/// `$HOME/<subdirectory>`, or the error that names everything that was tried.
fn unix_home_subdirectory(
    env: Lookup<'_>,
    subdirectory: &str,
    consulted: &'static [&'static str],
) -> Result<PathBuf, Unlocated> {
    match env("HOME").map(PathBuf::from) {
        Some(home) if home.is_absolute() => Ok(home.join(subdirectory)),
        _ => Err(Unlocated {
            consulted,
            remedy: Remedy::new(
                "Set one of them, for example:",
                "export XDG_CACHE_HOME=\"$HOME/.cache\"",
            ),
        }),
    }
}

/// `%LOCALAPPDATA%`, or `%USERPROFILE%\AppData\Local`, or the error.
fn windows_local_base(env: Lookup<'_>) -> Result<PathBuf, Unlocated> {
    if let Some(local) = env("LOCALAPPDATA").filter(is_absolute_windows) {
        return Ok(PathBuf::from(local));
    }
    match env("USERPROFILE").filter(is_absolute_windows) {
        Some(profile) => Ok(PathBuf::from(profile).join("AppData").join("Local")),
        None => Err(Unlocated {
            consulted: &["LOCALAPPDATA", "USERPROFILE"],
            remedy: Remedy::new(
                "Windows sets both for every account; in PowerShell, restore the one Clift needs:",
                "$env:LOCALAPPDATA = \"$env:USERPROFILE\\AppData\\Local\"",
            ),
        }),
    }
}

/// Whether a Windows path is absolute: a drive letter with a separator, or a
/// UNC path.
///
/// One rule, in one place: [`crate::domain::local_path::is_absolute`] answers
/// the same question for the paths `fetch` hands out, and two copies of it
/// would be two chances to be wrong on the platform neither is tested on.
fn is_absolute_windows(value: &OsString) -> bool {
    crate::domain::local_path::is_absolute(Platform::Windows, &value.to_string_lossy())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn env(pairs: &[(&str, &str)]) -> BTreeMap<String, OsString> {
        pairs
            .iter()
            .map(|(name, value)| ((*name).to_string(), OsString::from(value)))
            .collect()
    }

    /// Path comparison that does not care which separator the host used to
    /// join, so a Windows layout can be checked on a Unix test runner.
    fn segments(path: &Path) -> Vec<String> {
        path.to_string_lossy()
            .split(['/', '\\'])
            .filter(|segment| !segment.is_empty())
            .map(str::to_string)
            .collect()
    }

    fn lookup(map: &BTreeMap<String, OsString>) -> impl Fn(&str) -> Option<OsString> + '_ {
        move |name| map.get(name).filter(|value| !value.is_empty()).cloned()
    }

    #[test]
    fn unix_config_prefers_xdg_then_home() {
        let xdg = env(&[("XDG_CONFIG_HOME", "/x/cfg"), ("HOME", "/home/dev")]);
        assert_eq!(
            config_file(Platform::Unix, &lookup(&xdg)).unwrap(),
            PathBuf::from("/x/cfg/clift/config.toml")
        );
        let home = env(&[("HOME", "/home/dev"), ("XDG_CONFIG_HOME", "")]);
        assert_eq!(
            config_file(Platform::Unix, &lookup(&home)).unwrap(),
            PathBuf::from("/home/dev/.config/clift/config.toml")
        );
    }

    #[test]
    fn unix_without_home_names_the_variables_and_a_shell_command() {
        let none = env(&[]);
        let error = config_file(Platform::Unix, &lookup(&none))
            .unwrap_err()
            .into_error(Stage::Config, ErrorKind::Config, "the config file");
        let text = error.to_string();
        assert!(text.contains("XDG_CONFIG_HOME, HOME"), "{text}");
        let remedy = error.remedy().expect("a remedy").command().to_string();
        assert!(remedy.starts_with("export "), "{remedy}");
    }

    #[test]
    fn windows_config_lives_under_appdata() {
        let appdata = env(&[
            ("APPDATA", "C:\\Users\\jin\\AppData\\Roaming"),
            ("USERPROFILE", "C:\\Users\\jin"),
        ]);
        assert_eq!(
            segments(&config_file(Platform::Windows, &lookup(&appdata)).unwrap()),
            [
                "C:",
                "Users",
                "jin",
                "AppData",
                "Roaming",
                "Clift",
                "config.toml"
            ]
        );
        let profile_only = env(&[("USERPROFILE", "C:\\Users\\jin")]);
        assert_eq!(
            segments(&config_file(Platform::Windows, &lookup(&profile_only)).unwrap()),
            [
                "C:",
                "Users",
                "jin",
                "AppData",
                "Roaming",
                "Clift",
                "config.toml"
            ]
        );
    }

    #[test]
    fn windows_ignores_the_unix_variables_entirely() {
        // A HOME set by Git Bash or MSYS must not pull the config into a
        // dot-directory that no Windows tool looks at.
        let mixed = env(&[
            ("HOME", "C:\\Users\\jin"),
            ("XDG_CONFIG_HOME", "C:\\Users\\jin\\.config"),
            ("APPDATA", "C:\\Users\\jin\\AppData\\Roaming"),
            ("LOCALAPPDATA", "C:\\Users\\jin\\AppData\\Local"),
        ]);
        assert_eq!(
            segments(&config_file(Platform::Windows, &lookup(&mixed)).unwrap())[4],
            "Roaming"
        );
        assert_eq!(
            segments(&cache_dir(Platform::Windows, &lookup(&mixed)).unwrap()),
            ["C:", "Users", "jin", "AppData", "Local", "Clift"]
        );
        assert_eq!(
            run_dir(Platform::Windows, &lookup(&mixed)).unwrap(),
            cache_dir(Platform::Windows, &lookup(&mixed)).unwrap()
        );
    }

    #[test]
    fn windows_without_appdata_gets_a_powershell_command() {
        let none = env(&[("HOME", "/home/dev")]);
        let error = config_file(Platform::Windows, &lookup(&none))
            .unwrap_err()
            .into_error(Stage::Config, ErrorKind::Config, "the config file");
        let text = error.to_string();
        assert!(text.contains("APPDATA, USERPROFILE"), "{text}");
        let remedy = error.remedy().expect("a remedy").command().to_string();
        assert!(remedy.starts_with("$env:APPDATA"), "{remedy}");
        assert!(!remedy.contains("export"), "{remedy}");

        let error = cache_dir(Platform::Windows, &lookup(&none))
            .unwrap_err()
            .into_error(Stage::Staging, ErrorKind::RemoteDirectory, "the inbox");
        let remedy = error.remedy().expect("a remedy").command().to_string();
        assert!(remedy.starts_with("$env:LOCALAPPDATA"), "{remedy}");
    }

    #[test]
    fn a_relative_windows_value_is_not_trusted() {
        let relative = env(&[
            ("APPDATA", "AppData\\Roaming"),
            ("USERPROFILE", "C:\\Users\\jin"),
        ]);
        assert_eq!(
            segments(&config_file(Platform::Windows, &lookup(&relative)).unwrap())[0],
            "C:"
        );
        for absolute in ["C:\\Users\\jin", "c:/users/jin", "\\\\server\\share\\jin"] {
            assert!(is_absolute_windows(&OsString::from(absolute)), "{absolute}");
        }
        for relative in ["Users\\jin", "C:Users", "", "/home/dev"] {
            assert!(
                !is_absolute_windows(&OsString::from(relative)),
                "{relative}"
            );
        }
    }

    #[test]
    fn unix_run_dir_prefers_the_runtime_directory_then_the_cache_then_home() {
        let all = env(&[
            ("XDG_RUNTIME_DIR", "/run/user/1000"),
            ("XDG_CACHE_HOME", "/home/dev/.cache"),
            ("HOME", "/home/dev"),
        ]);
        assert_eq!(
            run_dir(Platform::Unix, &lookup(&all)).unwrap(),
            PathBuf::from("/run/user/1000/clift")
        );
        let no_runtime = env(&[("XDG_CACHE_HOME", "/x/cache"), ("HOME", "/home/dev")]);
        assert_eq!(
            run_dir(Platform::Unix, &lookup(&no_runtime)).unwrap(),
            PathBuf::from("/x/cache/clift")
        );
        let home_only = env(&[("HOME", "/home/dev")]);
        assert_eq!(
            run_dir(Platform::Unix, &lookup(&home_only)).unwrap(),
            PathBuf::from("/home/dev/.cache/clift")
        );
        assert_eq!(
            cache_dir(Platform::Unix, &lookup(&home_only)).unwrap(),
            PathBuf::from("/home/dev/.cache/clift")
        );
    }

    #[test]
    fn a_public_or_relative_nomination_is_skipped_not_followed() {
        for public in ["/tmp", "/var/tmp", "/dev/shm", "/tmp/anything"] {
            assert!(is_public(Path::new(public)), "{public}");
        }
        for private in ["/home/dev/.cache", "/run/user/1000", "/tmpfiles"] {
            assert!(!is_public(Path::new(private)), "{private}");
        }
        let public = env(&[("XDG_CACHE_HOME", "/tmp/cache"), ("HOME", "/home/dev")]);
        assert_eq!(
            cache_dir(Platform::Unix, &lookup(&public)).unwrap(),
            PathBuf::from("/home/dev/.cache/clift")
        );
        let relative = env(&[("XDG_RUNTIME_DIR", "run"), ("HOME", "/home/dev")]);
        assert_eq!(
            run_dir(Platform::Unix, &lookup(&relative)).unwrap(),
            PathBuf::from("/home/dev/.cache/clift")
        );
        let relative_home = env(&[("HOME", "dev")]);
        assert!(cache_dir(Platform::Unix, &lookup(&relative_home)).is_err());
    }

    #[test]
    fn the_current_platform_matches_the_build() {
        assert_eq!(
            Platform::current() == Platform::Windows,
            cfg!(windows),
            "Platform::current() must follow the target"
        );
    }
}
