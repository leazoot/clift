//! Remote directory operations against a real SFTP server.
//!
//! These are the primitives the staging layer is built on, so the tests care
//! less about the happy path than about the two rules that make the staging
//! layer safe: permissions are checked rather than corrected, and a symbolic
//! link is unlinked rather than followed.

#![allow(clippy::unwrap_used)]

#[path = "../../../tests/e2e/fixtures.rs"]
mod fixtures;

use clift_core::domain::RemotePath;
use clift_core::ports::{RemoteEntryKind, TransportTarget};
use clift_transport::probe::OpenSshTransport;
use clift_transport::proc::SshRunner;
use fixtures::{SshdFixture, Topology, skip_without_docker};
use std::time::{SystemTime, UNIX_EPOCH};

fn transport(fixture: &SshdFixture) -> OpenSshTransport {
    OpenSshTransport::with_runner(SshRunner::new().with_config_file(fixture.ssh_config()))
}

fn path(fixture: &SshdFixture, suffix: &str) -> RemotePath {
    RemotePath::new(format!("{}/{suffix}", fixture.remote_home())).unwrap()
}

fn remote_mode(fixture: &SshdFixture, path: &RemotePath) -> String {
    let output = fixture.ssh(&format!("stat -c %a \"{path}\""));
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

/// The inode change time, to nanosecond precision.
///
/// A `chmod` moves it even when the bits it writes are the ones already there,
/// which is what makes it able to see a permission change no mode comparison
/// could.
fn remote_ctime(fixture: &SshdFixture, path: &RemotePath) -> String {
    let output = fixture.ssh(&format!("stat -c %z \"{path}\""));
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

#[test]
fn the_remote_home_is_read_from_the_sftp_session_itself() {
    if skip_without_docker("the_remote_home_is_read_from_the_sftp_session_itself") {
        return;
    }
    let fixture = SshdFixture::start(Topology::Normal);
    let home = transport(&fixture)
        .resolve_home(&TransportTarget::new(fixture.alias()))
        .unwrap();
    assert_eq!(home.as_str(), fixture.remote_home());
}

#[test]
fn ensure_dir_creates_the_whole_chain_with_exactly_the_requested_mode() {
    if skip_without_docker("ensure_dir_creates_the_whole_chain_with_exactly_the_requested_mode") {
        return;
    }
    let fixture = SshdFixture::start(Topology::Normal);
    let transport = transport(&fixture);
    let target = TransportTarget::new(fixture.alias());

    let deep = path(&fixture, ".cache/clift/inbox/2026-08-30/批次 01");
    transport.ensure_dir(&target, &deep, 0o700).unwrap();

    for suffix in [
        ".cache/clift",
        ".cache/clift/inbox",
        ".cache/clift/inbox/2026-08-30",
        ".cache/clift/inbox/2026-08-30/批次 01",
    ] {
        assert_eq!(
            remote_mode(&fixture, &path(&fixture, suffix)),
            "700",
            "{suffix} was not created with mode 0700"
        );
    }

    // `.cache` already existed in the image; an ancestor that is not Clift's is
    // left exactly as it was.
    assert_eq!(remote_mode(&fixture, &path(&fixture, ".cache")), "700");

    // Idempotent: running it again must accept what is already there.
    transport.ensure_dir(&target, &deep, 0o700).unwrap();
}

#[test]
fn an_existing_directory_with_the_wrong_mode_is_refused_and_left_alone() {
    if skip_without_docker("an_existing_directory_with_the_wrong_mode_is_refused_and_left_alone") {
        return;
    }
    let fixture = SshdFixture::start(Topology::Normal);
    let inbox = path(&fixture, "loose");
    assert!(
        fixture
            .ssh("mkdir -p \"$HOME/loose\" && chmod 755 \"$HOME/loose\"")
            .status
            .success()
    );

    let error = transport(&fixture)
        .ensure_dir(&TransportTarget::new(fixture.alias()), &inbox, 0o700)
        .expect_err("a directory with looser permissions must not be accepted");

    assert_eq!(error.exit_code().as_u8(), 25);
    assert!(error.message().contains("0755"), "{error}");
    assert!(error.message().contains("0700"), "{error}");
    assert!(
        error.remedy().is_some(),
        "the user needs to be told what to do about it"
    );
    assert_eq!(
        remote_mode(&fixture, &inbox),
        "755",
        "Clift must not have changed the permissions it complained about"
    );
}

/// The mode is not the only thing a `chmod` writes.
///
/// `ensure_dir` asks `mkdir` first and reads "did it already exist" off its
/// failure. Misreading that as "I created it" would issue a `chmod` on a
/// directory Clift did not create -- and when the mode already is the one
/// Clift wants, no comparison of modes could ever notice. The change time can:
/// `chmod` moves it even when the bits it writes are the bits already there.
#[test]
fn an_existing_directory_is_not_chmod_ed_even_when_its_mode_is_already_right() {
    if skip_without_docker(
        "an_existing_directory_is_not_chmod_ed_even_when_its_mode_is_already_right",
    ) {
        return;
    }
    let fixture = SshdFixture::start(Topology::Normal);
    let inbox = path(&fixture, "already");
    assert!(
        fixture
            .ssh("mkdir -p \"$HOME/already\" && chmod 700 \"$HOME/already\"")
            .status
            .success()
    );
    let before = remote_ctime(&fixture, &inbox);
    assert!(!before.is_empty(), "the fixture must report a change time");

    transport(&fixture)
        .ensure_dir(&TransportTarget::new(fixture.alias()), &inbox, 0o700)
        .expect("a directory that is already exactly right is not an error");

    assert_eq!(
        remote_ctime(&fixture, &inbox),
        before,
        "Clift touched the permissions of a directory it did not create"
    );
}

#[test]
fn a_file_where_a_directory_belongs_is_refused() {
    if skip_without_docker("a_file_where_a_directory_belongs_is_refused") {
        return;
    }
    let fixture = SshdFixture::start(Topology::Normal);
    assert!(fixture.ssh("touch \"$HOME/occupied\"").status.success());

    let error = transport(&fixture)
        .ensure_dir(
            &TransportTarget::new(fixture.alias()),
            &path(&fixture, "occupied"),
            0o700,
        )
        .expect_err("a regular file must not pass as a directory");
    assert_eq!(error.exit_code().as_u8(), 25);
    assert!(error.message().contains("not a directory"), "{error}");
}

#[test]
fn stat_reports_size_mode_and_a_usable_modification_time() {
    if skip_without_docker("stat_reports_size_mode_and_a_usable_modification_time") {
        return;
    }
    let fixture = SshdFixture::start(Topology::Normal);
    let transport = transport(&fixture);
    let target = TransportTarget::new(fixture.alias());

    assert!(
        transport
            .stat(&target, &path(&fixture, "nothing here"))
            .unwrap()
            .is_none(),
        "a missing path is None, not an error"
    );

    assert!(
        fixture
            .ssh("printf '0123456789' > \"$HOME/文件 名.png\" && chmod 600 \"$HOME/文件 名.png\"")
            .status
            .success()
    );
    let entry = transport
        .stat(&target, &path(&fixture, "文件 名.png"))
        .unwrap()
        .expect("the file exists");
    assert_eq!(entry.name.as_str(), "文件 名.png");
    assert_eq!(entry.kind, RemoteEntryKind::File);
    assert_eq!(entry.size, 10);
    assert_eq!(entry.mode, Some(0o600));

    // Retention-based cleanup needs an absolute instant, not a rendered string.
    let modified = entry.modified.expect("a modification time is required");
    let modified = modified.duration_since(UNIX_EPOCH).unwrap().as_secs();
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    assert!(
        now.abs_diff(modified) < 600,
        "the file was just created but its mtime reads as {modified} against a clock of {now}"
    );
}

#[test]
fn a_listing_includes_dotfiles_and_excludes_the_directory_entries() {
    if skip_without_docker("a_listing_includes_dotfiles_and_excludes_the_directory_entries") {
        return;
    }
    let fixture = SshdFixture::start(Topology::Normal);
    let transport = transport(&fixture);
    let target = TransportTarget::new(fixture.alias());

    let directory = path(&fixture, "batch 01");
    transport.ensure_dir(&target, &directory, 0o700).unwrap();
    assert!(
        fixture
            .ssh("cd \"$HOME/batch 01\" && touch 'shot 1.png' .partial && mkdir sub")
            .status
            .success()
    );

    let mut names: Vec<String> = transport
        .list_dir(&target, &directory)
        .unwrap()
        .iter()
        .map(|entry| entry.name.as_str().to_string())
        .collect();
    names.sort();
    assert_eq!(
        names,
        vec![
            ".partial".to_string(),
            "shot 1.png".to_string(),
            "sub".to_string()
        ],
        "'.' and '..' must be filtered, but Clift's own dotfiles must not be"
    );

    let entries = transport.list_dir(&target, &directory).unwrap();
    let sub = entries.iter().find(|e| e.name.as_str() == "sub").unwrap();
    assert_eq!(sub.kind, RemoteEntryKind::Directory);
    assert!(entries.iter().all(|entry| entry.modified.is_some()));
}

#[test]
fn remove_unlinks_a_symlink_instead_of_following_it() {
    if skip_without_docker("remove_unlinks_a_symlink_instead_of_following_it") {
        return;
    }
    let fixture = SshdFixture::start(Topology::Normal);
    let transport = transport(&fixture);
    let target = TransportTarget::new(fixture.alias());

    assert!(
        fixture
            .ssh(
                "mkdir -p \"$HOME/outside\" && touch \"$HOME/outside/precious\" && \
                  ln -s \"$HOME/outside\" \"$HOME/escape\""
            )
            .status
            .success()
    );

    let link = path(&fixture, "escape");
    let entry = transport.stat(&target, &link).unwrap().unwrap();
    assert_eq!(
        entry.kind,
        RemoteEntryKind::Symlink,
        "a link must be reported as a link, not as the thing it points at"
    );

    transport.remove(&target, &link).unwrap();
    assert!(transport.stat(&target, &link).unwrap().is_none());
    assert!(
        fixture
            .ssh("test -f \"$HOME/outside/precious\"")
            .status
            .success(),
        "removing the link must not have touched what it pointed at"
    );
}

#[test]
fn remove_handles_files_empty_directories_and_paths_that_are_already_gone() {
    if skip_without_docker("remove_handles_files_empty_directories_and_paths_that_are_already_gone")
    {
        return;
    }
    let fixture = SshdFixture::start(Topology::Normal);
    let transport = transport(&fixture);
    let target = TransportTarget::new(fixture.alias());

    assert!(
        fixture
            .ssh("touch \"$HOME/文件 1.png\" && mkdir \"$HOME/空 目录\"")
            .status
            .success()
    );

    transport
        .remove(&target, &path(&fixture, "文件 1.png"))
        .unwrap();
    transport
        .remove(&target, &path(&fixture, "空 目录"))
        .unwrap();
    // Cleanup runs repeatedly; a path that is already gone is success.
    transport
        .remove(&target, &path(&fixture, "空 目录"))
        .unwrap();

    let listing = fixture.ssh("ls -1a \"$HOME\"");
    let listing = String::from_utf8_lossy(&listing.stdout);
    assert!(!listing.contains("文件 1.png"), "{listing}");
    assert!(!listing.contains("空 目录"), "{listing}");
}

#[test]
fn an_unwritable_home_fails_with_the_remote_directory_exit_code() {
    if skip_without_docker("an_unwritable_home_fails_with_the_remote_directory_exit_code") {
        return;
    }
    let fixture = SshdFixture::start(Topology::ReadonlyHome);
    let error = transport(&fixture)
        .ensure_dir(
            &TransportTarget::new(fixture.alias()),
            &path(&fixture, ".cache/clift/inbox"),
            0o700,
        )
        .expect_err("a read-only home cannot hold an inbox");
    assert_eq!(error.exit_code().as_u8(), 25);
    assert!(
        error.message().to_lowercase().contains("permission denied"),
        "the server's own reason must survive: {error}"
    );
}

/// Regression: five callers creating sibling directories under one missing
/// parent used to leave four of them with a bare `Failure`.
///
/// SFTP answers a `mkdir` of an existing directory with the same opaque
/// `Failure` it uses for everything else, so the loser of the race cannot tell
/// what happened from the message alone. Concurrent sends on the same day share
/// exactly one such parent -- the date directory -- which made this the normal
/// case rather than an exotic one.
#[test]
fn concurrent_callers_sharing_a_missing_parent_all_succeed() {
    if skip_without_docker("concurrent_callers_sharing_a_missing_parent_all_succeed") {
        return;
    }
    let fixture = SshdFixture::start(Topology::Normal);
    let transport = transport(&fixture);
    let target = TransportTarget::new(fixture.alias());
    let shared = path(&fixture, "race/2026-08-30");

    std::thread::scope(|scope| {
        for index in 0..5 {
            let transport = &transport;
            let target = &target;
            let shared = &shared;
            scope.spawn(move || {
                let child = RemotePath::new(format!("{shared}/batch-{index}")).unwrap();
                transport
                    .ensure_dir(target, &child, 0o700)
                    .unwrap_or_else(|error| panic!("caller {index} lost the race: {error}"));
            });
        }
    });

    for index in 0..5 {
        let child = RemotePath::new(format!("{shared}/batch-{index}")).unwrap();
        assert_eq!(remote_mode(&fixture, &child), "700");
    }
    assert_eq!(
        remote_mode(&fixture, &shared),
        "700",
        "the shared parent must still end up private"
    );
}
