//! Atomic write and migration behaviour of `config.toml`.

// A panic here is a test failure, not a user-facing crash; see clippy.toml.
#![allow(clippy::unwrap_used)]

use clift_core::config;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

static COUNTER: AtomicU32 = AtomicU32::new(0);

/// A scratch directory that removes itself, so a failing test cannot leave
/// files behind in the system temp directory.
struct Scratch(PathBuf);

impl Scratch {
    fn new(label: &str) -> Self {
        let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "clift-test-{}-{label}-{unique}",
            std::process::id()
        ));
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }

    fn config_path(&self) -> PathBuf {
        self.0.join("clift").join("config.toml")
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = fs::set_permissions(&self.0, fs::Permissions::from_mode(0o700));
            let _ = fs::set_permissions(self.0.join("clift"), fs::Permissions::from_mode(0o700));
        }
        let _ = fs::remove_dir_all(&self.0);
    }
}

const SAMPLE: &str = r#"
version = 1
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
remote_home = "/home/dev"
last_success_at = "2026-08-30T12:00:00Z"

[targets."my host"]
ssh_host = "other"
"#;

#[test]
fn a_missing_file_loads_as_an_empty_config() {
    let scratch = Scratch::new("missing");
    let loaded = config::io::load(&scratch.config_path()).unwrap();
    assert!(loaded.config.targets().is_empty());
    assert!(loaded.warnings.is_empty());
}

#[test]
fn save_then_load_round_trips_every_field() {
    let scratch = Scratch::new("roundtrip");
    let path = scratch.config_path();
    let original = config::parse(SAMPLE).unwrap().config;

    config::io::save(&path, &original).unwrap();
    let reloaded = config::io::load(&path).unwrap();

    assert!(reloaded.warnings.is_empty(), "{:?}", reloaded.warnings);
    assert_eq!(
        reloaded.config, original,
        "a saved config must load back identically"
    );
}

#[test]
fn saving_twice_is_stable() {
    let scratch = Scratch::new("stable");
    let path = scratch.config_path();
    let config = config::parse(SAMPLE).unwrap().config;

    config::io::save(&path, &config).unwrap();
    let first = fs::read_to_string(&path).unwrap();
    config::io::save(&path, &config).unwrap();
    let second = fs::read_to_string(&path).unwrap();

    assert_eq!(first, second, "writing the same config must be idempotent");
}

#[cfg(unix)]
#[test]
fn the_written_file_and_directory_are_private_to_the_user() {
    use std::os::unix::fs::PermissionsExt;

    let scratch = Scratch::new("perms");
    let path = scratch.config_path();
    config::io::save(&path, &config::parse(SAMPLE).unwrap().config).unwrap();

    let file_mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
    assert_eq!(file_mode, 0o600, "config file mode is {file_mode:o}");

    let dir_mode = fs::metadata(path.parent().unwrap())
        .unwrap()
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(dir_mode, 0o700, "config directory mode is {dir_mode:o}");
}

#[cfg(unix)]
#[test]
fn a_failed_write_leaves_the_previous_file_intact() {
    use std::os::unix::fs::PermissionsExt;

    let scratch = Scratch::new("interrupted");
    let path = scratch.config_path();
    let config = config::parse(SAMPLE).unwrap().config;
    config::io::save(&path, &config).unwrap();
    let before = fs::read(&path).unwrap();

    // A read-only directory makes creating the temporary file fail, which is
    // the same failure window as a crash between "created" and "renamed".
    let directory = path.parent().unwrap().to_path_buf();
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o500)).unwrap();

    let changed = config::parse("version = 1\n").unwrap().config;
    let error = config::io::save(&path, &changed).unwrap_err();
    assert_eq!(error.exit_code().as_u8(), 20);

    fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).unwrap();
    assert_eq!(
        fs::read(&path).unwrap(),
        before,
        "the previous config must survive a failed write byte for byte"
    );
    assert_eq!(
        config::io::load(&path).unwrap().config,
        config,
        "and must still parse to the same configuration"
    );
}

#[cfg(unix)]
#[test]
fn a_failed_write_leaves_no_temporary_file_behind() {
    use std::os::unix::fs::PermissionsExt;

    let scratch = Scratch::new("notemp");
    let path = scratch.config_path();
    let config = config::parse(SAMPLE).unwrap().config;
    config::io::save(&path, &config).unwrap();

    let directory = path.parent().unwrap().to_path_buf();
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o500)).unwrap();
    let _ = config::io::save(&path, &config);
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).unwrap();

    let leftovers: Vec<_> = fs::read_dir(&directory)
        .unwrap()
        .filter_map(Result::ok)
        .map(|entry| entry.file_name().to_string_lossy().to_string())
        .filter(|name| name.ends_with(".tmp"))
        .collect();
    assert!(leftovers.is_empty(), "left behind {leftovers:?}");
}

#[test]
fn a_config_without_a_version_is_migrated_and_reported() {
    let scratch = Scratch::new("migrate");
    let path = scratch.config_path();
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(
        &path,
        "default_target = \"core\"\n\n[targets.core]\nssh_host = \"core\"\n",
    )
    .unwrap();

    let loaded = config::io::load(&path).unwrap();
    assert_eq!(loaded.config.version(), 1);
    assert_eq!(loaded.config.default_target().unwrap().as_str(), "core");
    assert_eq!(loaded.warnings.len(), 1, "{:?}", loaded.warnings);
    assert!(
        loaded.warnings[0].contains("version"),
        "{:?}",
        loaded.warnings
    );
}

#[test]
fn a_newer_config_is_refused_without_rewriting_the_file() {
    let scratch = Scratch::new("future");
    let path = scratch.config_path();
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let source = "version = 999\n";
    fs::write(&path, source).unwrap();

    let error = config::io::load(&path).unwrap_err();
    assert_eq!(error.exit_code().as_u8(), 20);
    assert_eq!(
        fs::read_to_string(&path).unwrap(),
        source,
        "a refused config must not be downgraded on disk"
    );
}

#[test]
fn the_default_path_honours_xdg_config_home() {
    // SAFETY-adjacent note: environment mutation is process wide, so this test
    // sets the variable and reads it back without spawning threads.
    let previous = std::env::var_os("XDG_CONFIG_HOME");
    unsafe { std::env::set_var("XDG_CONFIG_HOME", "/tmp/clift-xdg") };
    let path = config::io::default_config_path().unwrap();
    match previous {
        Some(value) => unsafe { std::env::set_var("XDG_CONFIG_HOME", value) },
        None => unsafe { std::env::remove_var("XDG_CONFIG_HOME") },
    }
    assert_eq!(path, PathBuf::from("/tmp/clift-xdg/clift/config.toml"));
}
