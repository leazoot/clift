//! `clift send` end to end, against a real OpenSSH server.
//!
//! The claims worth testing here are the ones a unit test cannot make: that the
//! files really arrive, really have mode 0600, and that when one of them does
//! not arrive **no path at all** comes back.

#![allow(clippy::unwrap_used, clippy::expect_used)]

#[path = "../../../tests/e2e/fixtures.rs"]
mod fixtures;

use fixtures::{SshdFixture, Topology, skip_without_docker};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// A run of the real binary against the fixture container.
///
/// `PATH` shims point the real clients at the container's configuration, which
/// is how a test steers `ssh` without the product needing a back door.
struct Sandbox {
    home: PathBuf,
    bin: PathBuf,
    work: PathBuf,
}

impl Sandbox {
    fn new(fixture: &SshdFixture, label: &str) -> Self {
        let home = fixture.workdir().join(format!("send-home-{label}"));
        let bin = fixture.workdir().join(format!("send-bin-{label}"));
        let work = fixture.workdir().join(format!("send-work-{label}"));
        for path in [&home, &bin, &work] {
            fs::create_dir_all(path).unwrap();
        }
        shim(&bin, fixture);

        let sandbox = Self { home, bin, work };
        assert!(
            sandbox
                .run(&["setup", fixture.alias(), "--yes"])
                .status
                .success(),
            "the fixture host must set up cleanly"
        );
        sandbox
    }

    fn file(&self, name: &str, contents: &[u8]) -> PathBuf {
        let path = self.work.join(name);
        fs::write(&path, contents).unwrap();
        path
    }

    fn run(&self, args: &[&str]) -> Output {
        self.command(args).output().expect("clift must be runnable")
    }

    /// Runs with the working directory set inside the sandbox, so that a
    /// relative path means something.
    fn run_in_work(&self, args: &[&str]) -> Output {
        self.command(args)
            .current_dir(&self.work)
            .output()
            .expect("clift must be runnable")
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_clift"));
        command
            .args(args)
            .env("XDG_CONFIG_HOME", &self.home)
            .env(
                "PATH",
                format!(
                    "{}:{}",
                    self.bin.display(),
                    std::env::var("PATH").unwrap_or_default()
                ),
            )
            .env("NO_COLOR", "1");
        command
    }
}

fn shim(bin: &Path, fixture: &SshdFixture) {
    for client in ["ssh", "sftp"] {
        let located = Command::new("/usr/bin/which")
            .arg(client)
            .output()
            .expect("which must be runnable");
        let real = String::from_utf8_lossy(&located.stdout).trim().to_string();
        let path = bin.join(client);
        fs::write(
            &path,
            format!(
                "#!/bin/sh\nexec {real} -F {} \"$@\"\n",
                fixture.ssh_config().display()
            ),
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        }
    }
}

fn stdout_of(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr_of(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

/// The paths out of an insertion text, unquoted.
///
/// The reverse of the formatter's quoting, including its escape for an embedded
/// single quote (`'\''`). Splitting on quote characters would tear those apart,
/// which is how this helper got it wrong the first time.
fn paths_in(text: &str) -> Vec<String> {
    let mut paths = Vec::new();
    for line in text.lines() {
        let quoted = line
            .strip_prefix("Please inspect this file: ")
            .or_else(|| line.strip_prefix("- "));
        let Some(quoted) = quoted else {
            continue;
        };
        let inner = quoted
            .strip_prefix('\'')
            .and_then(|rest| rest.strip_suffix('\''))
            .unwrap_or(quoted);
        paths.push(inner.replace("'\\''", "'"));
    }
    paths
}

fn remote(fixture: &SshdFixture, command: &str) -> String {
    let output = fixture.ssh(command);
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

/// Single-quotes a path for the remote shell.
///
/// Double quotes would let `$HOME` in a file name expand on the far side, which
/// is how this helper first reported that a file "did not arrive" when it had.
fn shell_quote(path: &str) -> String {
    format!("'{}'", path.replace('\'', "'\\''"))
}

#[test]
fn one_file_arrives_private_and_its_path_comes_back() {
    if skip_without_docker("one_file_arrives_private_and_its_path_comes_back") {
        return;
    }
    let fixture = SshdFixture::start(Topology::Normal);
    let sandbox = Sandbox::new(&fixture, "one");
    let source = sandbox.file("shot.png", b"the picture");

    let output = sandbox.run(&["send", source.to_str().unwrap()]);
    assert!(output.status.success(), "{}", stderr_of(&output));

    let text = stdout_of(&output);
    assert!(
        text.starts_with("Please inspect this file: '"),
        "stdout must be exactly the insertion text: {text:?}"
    );
    let paths = paths_in(&text);
    assert_eq!(paths.len(), 1, "{text}");
    assert!(paths[0].starts_with('/'), "paths must be absolute: {text}");

    assert_eq!(
        remote(&fixture, &format!("cat {}", shell_quote(&paths[0]))),
        "the picture"
    );
    assert_eq!(
        remote(&fixture, &format!("stat -c %a {}", shell_quote(&paths[0]))),
        "600"
    );
    assert!(
        stderr_of(&output).contains("Sent 1 file"),
        "{}",
        stderr_of(&output)
    );
}

/// Five files, a relative path, a name with spaces and a name in Chinese.
#[test]
fn five_files_with_awkward_names_all_arrive() {
    if skip_without_docker("five_files_with_awkward_names_all_arrive") {
        return;
    }
    let fixture = SshdFixture::start(Topology::Normal);
    let sandbox = Sandbox::new(&fixture, "five");

    let names = [
        "plain.txt",
        "with spaces.txt",
        "名字.txt",
        "it's here.txt",
        "dollar $HOME.txt",
    ];
    for (index, name) in names.iter().enumerate() {
        sandbox.file(name, format!("contents {index}").as_bytes());
    }

    // Relative paths, resolved against the working directory.
    let output = sandbox.run_in_work(&["send", names[0], names[1], names[2], names[3], names[4]]);
    assert!(output.status.success(), "{}", stderr_of(&output));

    let text = stdout_of(&output);
    assert!(text.starts_with("Please inspect these files:"), "{text}");
    let paths = paths_in(&text);
    assert_eq!(paths.len(), 5, "{text}");

    for (index, path) in paths.iter().enumerate() {
        assert_eq!(
            remote(&fixture, &format!("cat {}", shell_quote(path))),
            format!("contents {index}"),
            "file {index} did not arrive intact"
        );
    }
    // All five in one batch directory.
    let directories: std::collections::BTreeSet<&str> = paths
        .iter()
        .map(|path| path.rsplit_once('/').unwrap().0)
        .collect();
    assert_eq!(directories.len(), 1, "one send is one batch: {paths:?}");
}

/// Acceptance: the machine document is what the specification promises, produced for real.
#[test]
fn the_json_document_is_the_v1_contract() {
    if skip_without_docker("the_json_document_is_the_v1_contract") {
        return;
    }
    let fixture = SshdFixture::start(Topology::Normal);
    let sandbox = Sandbox::new(&fixture, "json");
    let source = sandbox.file("shot.png", b"0123456789");

    let output = sandbox.run(&["--json", "send", source.to_str().unwrap()]);
    assert!(output.status.success(), "{}", stderr_of(&output));

    let text = stdout_of(&output);
    let value: serde_json::Value = serde_json::from_str(&text).expect("one JSON document");
    assert_eq!(
        text,
        serde_json::to_string(&value).unwrap(),
        "stdout carries bytes beyond the document"
    );
    assert_eq!(value["schema_version"], 1);
    assert_eq!(value["status"], "ok");
    assert_eq!(value["target"], fixture.alias());
    assert_eq!(value["items"][0]["mime"], "image/png");
    assert_eq!(value["items"][0]["size"], 10);
    assert!(
        value["items"][0]["remote_path"]
            .as_str()
            .unwrap()
            .starts_with('/')
    );
    assert!(
        value["insertion_text"]
            .as_str()
            .unwrap()
            .starts_with("Please inspect this file:")
    );
    // No local path, no attachment content.
    assert!(
        !text.contains("send-work-json"),
        "a local path leaked: {text}"
    );
    assert!(
        !text.contains("0123456789"),
        "the file's bytes leaked: {text}"
    );
}

/// A file that is not there is refused before anything is uploaded.
#[test]
fn a_missing_file_is_refused_before_the_host_is_touched() {
    if skip_without_docker("a_missing_file_is_refused_before_the_host_is_touched") {
        return;
    }
    let fixture = SshdFixture::start(Topology::Normal);
    let sandbox = Sandbox::new(&fixture, "missing");
    let good = sandbox.file("here.txt", b"x");

    let output = sandbox.run(&["send", good.to_str().unwrap(), "/nonexistent/never/was.png"]);
    assert_eq!(output.status.code(), Some(24), "{}", stderr_of(&output));
    assert!(
        output.stdout.is_empty(),
        "a failure must leave stdout empty"
    );

    let inbox = format!("{}/.cache/clift/inbox", fixture.remote_home());
    let listing = remote(&fixture, &format!("find \"{inbox}\" -type f"));
    assert!(
        listing.is_empty(),
        "something was uploaded anyway: {listing}"
    );
}

/// Five files, the third cannot be written, and **no path comes back**.
///
/// The failure is real -- the destination is on a 1 MiB filesystem and the
/// third file is 3 MiB -- not an injected one. The specification forbids proving this
/// with a mock transport.
#[test]
fn a_batch_whose_third_file_fails_yields_no_paths_at_all() {
    if skip_without_docker("a_batch_whose_third_file_fails_yields_no_paths_at_all") {
        return;
    }
    // The cache directory -- and so the inbox -- is a 1 MiB filesystem, which
    // is what makes an ordinary send run out of room part way through.
    let fixture = SshdFixture::start(Topology::SmallCache);
    let sandbox = Sandbox::new(&fixture, "atomic");
    let inbox = format!("{}/.cache/clift/inbox", fixture.remote_home());

    let mut names = Vec::new();
    for index in 0..5 {
        let name = format!("file-{index}.bin");
        let size = if index == 2 { 3 * 1024 * 1024 } else { 16 };
        sandbox.file(&name, &vec![b'x'; size]);
        names.push(name);
    }

    let args: Vec<&str> = std::iter::once("send")
        .chain(names.iter().map(String::as_str))
        .collect();
    let output = sandbox.run_in_work(&args);

    assert!(!output.status.success(), "the third file cannot fit");
    assert_eq!(
        output.status.code(),
        Some(23),
        "an incomplete write is a transfer failure: {}",
        stderr_of(&output)
    );
    assert!(
        output.stdout.is_empty(),
        "a partial batch must produce no path at all: {:?}",
        stdout_of(&output)
    );
    let stderr = stderr_of(&output);
    assert!(
        stderr.contains("transfer") || stderr.contains("upload"),
        "the failure must name the stage: {stderr}"
    );

    // Nothing survives on the host either: the batch cleaned up after itself.
    let listing = remote(&fixture, &format!("find \"{inbox}\" -type f"));
    assert!(
        listing.is_empty(),
        "a failed batch left files behind: {listing}"
    );

    // Retrying the same batch is possible: nothing about the failure is sticky.
    let smaller = sandbox.run_in_work(&["send", &names[0], &names[1]]);
    assert!(
        smaller.status.success(),
        "the user must be able to try again: {}",
        stderr_of(&smaller)
    );
}
