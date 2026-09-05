//! Universal Mode through the real binary, against a real relay.
//!
//! What is exercised here is the whole receiving half -- `clift fetch` as a
//! subprocess, a `clift-relayd` process on a real port, a real object sealed by
//! this test's own code -- and every contract the sending half can be checked
//! for without a clipboard.
//!
//! Two things are deliberately **not** claimed by any test in this file: that a
//! screenshot reaches a relay (that needs a real clipboard, and lives in
//! `send_clipboard.rs`'s pattern behind `CLIFT_REAL_CLIPBOARD`), and that a
//! keystroke reaches a terminal (that needs a person).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use clift_core::domain::SafeFileName;
use clift_core::ports::{ClipboardSource, Relay};
use clift_core::universal::crypto::NONCE_BYTES;
use clift_core::universal::token::{SEAL_KEY_BYTES, SealKey};
use clift_core::universal::{RelaySettings, Token, bundle, crypto};
use clift_relay::HttpRelay;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, Command, Output, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

static COUNTER: AtomicU32 = AtomicU32::new(0);
const MINUTE: Duration = Duration::from_secs(60);

fn announce(line: &str) {
    let mut stderr = std::io::stderr();
    let _ = writeln!(stderr, "{line}");
    let _ = stderr.flush();
}

/// See the note in `clift-relay/tests/real_relay.rs`: the daemon is a sibling
/// of the test binary, because `CARGO_BIN_EXE_` only covers the same package.
fn relayd_binary() -> Option<PathBuf> {
    let mut path = std::env::current_exe().ok()?;
    path.pop();
    if path.ends_with("deps") {
        path.pop();
    }
    let candidate = path.join(if cfg!(windows) {
        "clift-relayd.exe"
    } else {
        "clift-relayd"
    });
    candidate.is_file().then_some(candidate)
}

struct Relayd {
    child: Child,
    url: String,
}

impl Relayd {
    fn start() -> Option<Self> {
        let mut child = Command::new(relayd_binary()?)
            .env("CLIFT_RELAY_ADDR", "127.0.0.1:0")
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .ok()?;
        let mut reader = BufReader::new(child.stderr.take()?);
        let mut line = String::new();
        if reader.read_line(&mut line).ok()? == 0 {
            let _ = child.kill();
            return None;
        }
        let address = line
            .split("listening on ")
            .nth(1)?
            .split(' ')
            .next()?
            .trim();
        let url = format!("http://{address}");
        std::thread::spawn(move || {
            let mut sink = String::new();
            while reader.read_line(&mut sink).unwrap_or(0) > 0 {
                sink.clear();
            }
        });
        Some(Self { child, url })
    }

    /// Seals a bundle and leaves it with the relay, returning the token a user
    /// would have pasted. The sealing is the product's own code, so what the
    /// test invents is only the payload.
    fn publish(&self, name: &str, media_type: &str, data: &[u8]) -> Token {
        self.publish_many(&[(name, media_type, data)])
    }

    /// The same, for a batch. One object carries the whole batch, so this is
    /// still one token.
    fn publish_many(&self, items: &[(&str, &str, &[u8])]) -> Token {
        let entries: Vec<_> = items
            .iter()
            .map(|(name, media_type, data)| {
                bundle::BundleEntry::new(
                    SafeFileName::new(*name).expect("a valid test name"),
                    *media_type,
                    data.to_vec(),
                )
                .expect("a carried media type")
            })
            .collect();
        let frame = bundle::encode(&entries).expect("encode");

        let key = SealKey::from_bytes([0x5A; SEAL_KEY_BYTES]);
        let sealed = crypto::seal(&frame, &key, &[0x3C; NONCE_BYTES]).expect("seal");

        let settings = RelaySettings::new(&self.url, 8 * 1024 * 1024, MINUTE).expect("settings");
        let published = HttpRelay::new(&settings)
            .publish(&sealed, MINUTE)
            .expect("publish");
        Token::new(published.id, key)
    }
}

impl Drop for Relayd {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// A private HOME and config directory, so a test never touches the developer's
/// own inbox or configuration.
struct Scratch(PathBuf);

impl Scratch {
    fn new(label: &str) -> Self {
        let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "clift-universal-{}-{label}-{unique}",
            std::process::id()
        ));
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }

    fn run(&self, relay_url: Option<&str>, args: &[&str]) -> Output {
        let mut command = Command::new(env!("CARGO_BIN_EXE_clift"));
        command
            .args(args)
            .env("XDG_CONFIG_HOME", self.0.join("config"))
            .env("XDG_CACHE_HOME", self.0.join("cache"))
            .env("HOME", &self.0)
            .env("NO_COLOR", "1")
            .env_remove("CLIFT_RELAY_URL")
            .env_remove("CLIFT_RELAY_MAX_BYTES")
            .env_remove("CLIFT_RELAY_TTL");
        if let Some(url) = relay_url {
            command.env("CLIFT_RELAY_URL", url);
        }
        command.output().expect("the clift binary must be runnable")
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn stdout_of(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr_of(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn code_of(output: &Output) -> i32 {
    output.status.code().unwrap_or(-1)
}

macro_rules! relay_or_skip {
    ($name:literal) => {
        match Relayd::start() {
            Some(relay) => relay,
            None => {
                announce(concat!(
                    "SKIPPED ", $name,
                    ": clift-relayd is not built (cargo build -p clift-relayd); this test proved nothing"
                ));
                return;
            }
        }
    };
}

/// The whole receiving half: a real relay, a real token, the real binary, and a
/// file on disk at the path it printed.
#[test]
fn fetch_writes_the_attachment_and_prints_where_it_put_it() {
    let relay = relay_or_skip!("fetch_writes_the_attachment_and_prints_where_it_put_it");
    let scratch = Scratch::new("fetch");
    let token = relay.publish("shot.png", "image/png", b"pretend pixels");

    let output = scratch.run(Some(&relay.url), &["fetch", &token.expose()]);
    assert_eq!(code_of(&output), 0, "{}", stderr_of(&output));

    let printed = stdout_of(&output);
    let path = printed.trim();
    assert!(path.starts_with('/'), "not an absolute path: {printed:?}");
    assert!(path.ends_with("/shot.png"), "{printed:?}");
    assert_eq!(
        fs::read(path).expect("the file must exist"),
        b"pretend pixels"
    );

    // Exactly one line, and nothing else: an agent substitutes this into a
    // command.
    assert_eq!(printed.lines().count(), 1, "{printed:?}");
}

/// `clift copy`, the return trip, through the real binary and a real relay.
///
/// The assertion that matters is the shape of stdout: **one bare token, on one
/// line, with nothing wrapped around it**. That is not a stylistic preference.
/// The key press on the far end tells a token apart from the instruction
/// `paste --copy` leaves on a clipboard by accepting only a token on its own,
/// so an instruction printed here would be a token the other end refuses.
#[test]
fn copy_prints_one_bare_token_and_nothing_else() {
    let relay = relay_or_skip!("copy_prints_one_bare_token_and_nothing_else");
    let scratch = Scratch::new("copy");
    let file = scratch.0.join("report.png");
    fs::write(&file, b"pretend pixels").unwrap();

    let output = scratch.run(
        Some(&relay.url),
        &["copy", file.to_str().expect("a utf-8 path")],
    );
    assert_eq!(code_of(&output), 0, "{}", stderr_of(&output));

    let printed = stdout_of(&output);
    assert_eq!(printed.lines().count(), 1, "{printed:?}");
    let line = printed.trim();
    Token::parse(line).expect("stdout must be a token by itself");
    assert!(
        !line.contains("clift fetch") && !line.contains('\''),
        "the token was wrapped in an instruction: {line:?}"
    );
}

/// The whole return trip through two invocations of the real binary: sealed on
/// one machine, redeemed on the other, byte for byte.
///
/// Two separate `Scratch` homes, because that is the claim -- the second run
/// shares nothing with the first except the token it was handed and the relay,
/// which never sees the key.
#[test]
fn a_file_copied_on_one_machine_is_fetched_on_the_other() {
    let relay = relay_or_skip!("a_file_copied_on_one_machine_is_fetched_on_the_other");
    let sender = Scratch::new("copy-send");
    let receiver = Scratch::new("copy-receive");
    let file = sender.0.join("diagram.png");
    let bytes: Vec<u8> = (0..4096_u32).map(|i| (i % 251) as u8).collect();
    fs::write(&file, &bytes).unwrap();

    let published = sender.run(
        Some(&relay.url),
        &["copy", file.to_str().expect("a utf-8 path")],
    );
    assert_eq!(code_of(&published), 0, "{}", stderr_of(&published));
    let token = stdout_of(&published).trim().to_string();

    let redeemed = receiver.run(Some(&relay.url), &["fetch", &token]);
    assert_eq!(code_of(&redeemed), 0, "{}", stderr_of(&redeemed));
    let landed = stdout_of(&redeemed).trim().to_string();
    assert!(landed.ends_with("/diagram.png"), "{landed:?}");
    assert_eq!(fs::read(&landed).expect("the file must exist"), bytes);

    // Single use applies in this direction too, and it is the relay that says
    // so rather than either end remembering.
    let again = receiver.run(Some(&relay.url), &["fetch", &token]);
    assert_eq!(code_of(&again), 27, "{}", stderr_of(&again));
}

/// A host that has never been pointed at a relay gets the advice that fits the
/// machine it is on: use the other machine's relay, not "pick one".
#[test]
fn copy_without_a_relay_points_at_the_other_machines_relay() {
    let scratch = Scratch::new("copy-no-relay");
    let file = scratch.0.join("note.txt");
    fs::write(&file, b"text").unwrap();

    let output = scratch.run(None, &["copy", file.to_str().expect("a utf-8 path")]);
    assert_eq!(code_of(&output), 20, "{}", stderr_of(&output));
    assert!(
        output.stdout.is_empty(),
        "a failed copy printed something: {:?}",
        stdout_of(&output)
    );
    let stderr = stderr_of(&output);
    assert!(
        stderr.contains("clift config set relay.url"),
        "no way out was offered: {stderr}"
    );
    // The sending machine's wording would send this user to choose a relay,
    // when the only relay that helps is the one the other machine already uses.
    assert!(
        stderr.contains("the machine you paste on"),
        "the wrong end's advice was given: {stderr}"
    );
}

/// `clift copy` with nothing to copy is a usage error, not an empty publish.
#[test]
fn copy_needs_a_file_to_copy() {
    let scratch = Scratch::new("copy-no-file");
    let output = scratch.run(None, &["copy"]);
    assert_ne!(code_of(&output), 0);
    assert!(output.stdout.is_empty(), "{:?}", stdout_of(&output));
}

/// A token is single use, and the relay is what enforces it. The second
/// attempt must fail with the code that means "spent", not "broken".
#[test]
fn the_same_token_cannot_be_fetched_twice() {
    let relay = relay_or_skip!("the_same_token_cannot_be_fetched_twice");
    let scratch = Scratch::new("twice");
    let token = relay.publish("shot.png", "image/png", b"once");

    let first = scratch.run(
        Some(&relay.url),
        &["fetch", &token.expose(), "--print-path"],
    );
    assert_eq!(code_of(&first), 0, "{}", stderr_of(&first));

    let second = scratch.run(
        Some(&relay.url),
        &["fetch", &token.expose(), "--print-path"],
    );
    assert_eq!(code_of(&second), 27, "{}", stderr_of(&second));
    assert!(
        second.stdout.is_empty(),
        "a failed fetch printed a path: {:?}",
        stdout_of(&second)
    );
}

/// The all-or-nothing rule's promise, applied to the other direction: a fetch that fails prints
/// no path, so an agent is never handed a name for a file that is not there.
#[test]
fn every_way_a_fetch_can_fail_prints_nothing_on_stdout() {
    let relay = relay_or_skip!("every_way_a_fetch_can_fail_prints_nothing_on_stdout");
    let scratch = Scratch::new("failures");

    let real = relay.publish("shot.png", "image/png", b"pixels");
    let wrong_key = {
        let text = real.expose();
        let (head, _) = text.split_once('#').expect("a token has two halves");
        // A structurally valid key that is not the right one.
        format!(
            "{head}#{}",
            SealKey::from_bytes([1; SEAL_KEY_BYTES]).encoded()
        )
    };

    let cases: [(&str, &str, i32); 5] = [
        ("not a token at all", "hello", 27),
        (
            "a future version",
            "clift://v2/AAAAAAAAAAAAAAAAAAAAAA#AAAA",
            27,
        ),
        (
            "truncated in transit",
            &real.expose()[..real.expose().len() - 6],
            27,
        ),
        (
            "an object that is not there",
            "clift://v1/AAECAwQFBgcICQoLDA0ODw#AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8",
            27,
        ),
        ("the wrong key", &wrong_key, 29),
    ];

    for (label, token, expected) in cases {
        let output = scratch.run(Some(&relay.url), &["fetch", token]);
        assert_eq!(
            code_of(&output),
            expected,
            "{label}: {}",
            stderr_of(&output)
        );
        assert!(
            output.stdout.is_empty(),
            "{label}: something reached stdout: {:?}",
            stdout_of(&output)
        );
    }
}

/// The relay is down. That is a different failure from a spent token and must
/// have a different exit code, or a user restarts a service over an expiry.
#[test]
fn an_unreachable_relay_is_exit_code_twenty_eight() {
    let scratch = Scratch::new("down");
    let token = "clift://v1/AAECAwQFBgcICQoLDA0ODw#AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8";

    let output = scratch.run(
        Some("http://127.0.0.1:1"),
        &["fetch", token, "--print-path"],
    );
    assert_eq!(code_of(&output), 28, "{}", stderr_of(&output));
    assert!(output.stdout.is_empty());
    assert!(
        stderr_of(&output).contains("could not be reached"),
        "{}",
        stderr_of(&output)
    );
}

/// A token in a log is a token in somebody's scrollback. Not under `--debug`
/// either, which is the flag most likely to be on when it would matter.
#[test]
fn the_key_material_is_never_echoed_back() {
    let relay = relay_or_skip!("the_key_material_is_never_echoed_back");
    let scratch = Scratch::new("redaction");
    let token = relay.publish("shot.png", "image/png", b"pixels");
    let key = token.key().encoded();

    for arguments in [
        vec!["--debug", "fetch", &token.expose(), "--print-path"],
        vec!["--verbose", "fetch", &token.expose(), "--print-path"],
    ] {
        // A fresh object each time, since the first run consumes it.
        let token = relay.publish("shot.png", "image/png", b"pixels");
        let exposed = token.expose();
        let mut arguments = arguments;
        arguments[2] = &exposed;

        let output = scratch.run(Some(&relay.url), &arguments);
        let text = stderr_of(&output);
        assert!(
            !text.contains(&key) && !text.contains(&token.key().encoded()),
            "the key appeared on stderr: {text}"
        );
        assert!(
            text.contains("<redacted>") || !text.contains("clift://"),
            "a token was written out in full: {text}"
        );
    }
}

/// `--json` is one document and nothing else, and it carries no host: at this
/// point nobody has chosen one.
#[test]
fn the_fetch_document_is_one_json_object_naming_only_this_host() {
    let relay = relay_or_skip!("the_fetch_document_is_one_json_object_naming_only_this_host");
    let scratch = Scratch::new("json");
    let token = relay.publish("notes.md", "text/markdown", b"# heading");

    let output = scratch.run(Some(&relay.url), &["--json", "fetch", &token.expose()]);
    assert_eq!(code_of(&output), 0, "{}", stderr_of(&output));

    let document: serde_json::Value =
        serde_json::from_str(&stdout_of(&output)).expect("stdout must be exactly one document");
    assert_eq!(document["schema_version"], 1);
    assert_eq!(document["status"], "ok");
    let items = document["items"].as_array().expect("items");
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["mime"], "text/markdown");
    assert_eq!(items[0]["size"], 9);
    assert!(
        items[0]["path"]
            .as_str()
            .unwrap_or_default()
            .ends_with("/notes.md")
    );
    // No token, no key, no relay URL: the fetch is over and none of them is
    // anybody's business afterwards.
    let rendered = stdout_of(&output);
    assert!(!rendered.contains("clift://"), "{rendered}");
    assert!(!rendered.contains(&token.key().encoded()), "{rendered}");
}

/// Universal Mode with nowhere to publish to must say so, and say how to fix
/// it, rather than falling back to a default target and uploading somewhere the
/// user did not ask for.
#[test]
fn universal_mode_without_a_relay_refuses_and_never_falls_back() {
    let scratch = Scratch::new("norelay");
    fs::create_dir_all(scratch.0.join("config").join("clift")).unwrap();
    fs::write(
        scratch.0.join("config").join("clift").join("config.toml"),
        "version = 1\ndefault_target = \"core\"\n[targets.core]\nssh_host = \"core\"\n",
    )
    .unwrap();

    let output = scratch.run(
        None,
        &[
            "fetch",
            "clift://v1/AAECAwQFBgcICQoLDA0ODw#AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8",
        ],
    );
    assert_eq!(code_of(&output), 20, "{}", stderr_of(&output));
    let text = stderr_of(&output);
    assert!(text.contains("relay"), "{text}");
    assert!(text.contains("clift config set relay.url"), "{text}");
    // This is the receiving host's error, so it says where the address lives:
    // on the machine that sent the token, not in the token.
    assert!(text.contains("this host"), "{text}");
    assert!(text.contains("clift status"), "{text}");
    // Nothing was sent to the configured target instead.
    assert!(
        !text.contains("core"),
        "a target was involved anyway: {text}"
    );
}

/// `--to` names a host, and Universal Mode does not send to one. Ignoring the
/// flag would leave the user believing something untrue about where their file
/// went, so it is refused.
#[test]
fn universal_mode_refuses_a_target_rather_than_ignoring_it() {
    let scratch = Scratch::new("withto");
    let output = scratch.run(
        Some("http://127.0.0.1:1"),
        &["paste", "--mode", "universal", "--to", "core"],
    );
    assert_eq!(code_of(&output), 20, "{}", stderr_of(&output));
    let text = stderr_of(&output);
    assert!(text.contains("--to"), "{text}");
    assert!(
        text.contains("--mode fast"),
        "the way out is not offered: {text}"
    );
}

#[test]
fn an_unknown_mode_is_refused_rather_than_guessed_at() {
    let scratch = Scratch::new("badmode");
    let output = scratch.run(None, &["paste", "--mode", "univarsal"]);
    assert_eq!(code_of(&output), 20, "{}", stderr_of(&output));
    assert!(output.stdout.is_empty());
}

/// `--copy` and `--inject` are two answers to the same question, and clap
/// refuses both at once rather than picking one.
#[test]
fn copy_and_inject_cannot_both_be_asked_for() {
    let scratch = Scratch::new("bothflags");
    let output = scratch.run(None, &["paste", "--copy", "--inject"]);
    assert_ne!(code_of(&output), 0);
    assert!(output.stdout.is_empty());
}

/// Fast Mode must not have acquired a relay dependency. With no relay
/// configured anywhere, a Fast Mode paste has to fail for a Fast Mode reason.
#[test]
fn fast_mode_does_not_need_a_relay() {
    let scratch = Scratch::new("fastnorelay");
    let output = scratch.run(None, &["paste", "--mode", "fast"]);
    let text = stderr_of(&output);
    assert!(
        !text.contains("relay"),
        "Fast Mode mentioned the relay: {text}"
    );
    assert!(output.stdout.is_empty());
}

// --- The return trip: `fetch --copy` ---------------------------------------
//
// Every case here is one the clipboard is *not* written in, which is what makes
// them safe to run anywhere: they assert the refusals, and a refusal never
// touches what the developer had copied. The one case that does write is behind
// `CLIFT_REAL_CLIPBOARD` with the rest of them.

/// A spent token with `--copy` fails like any other spent token, prints nothing
/// that could be mistaken for success, and offers the way out that belongs to
/// the direction it was travelling.
///
/// The last part is the one worth a test. The relay client raises this failure
/// and cannot know the direction, so its own advice is "publish again from
/// here" -- which on a return trip would send whatever is on this clipboard to
/// somebody else. That advice was found by running the whole thing against a
/// real server before this assertion existed.
#[test]
fn a_spent_token_with_copy_points_back_at_the_machine_it_came_from() {
    let relay = relay_or_skip!("a_spent_token_with_copy_points_back_at_the_machine_it_came_from");
    let scratch = Scratch::new("copy-spent");
    let token = relay.publish("shot.png", "image/png", b"once");

    let first = scratch.run(Some(&relay.url), &["fetch", &token.expose()]);
    assert_eq!(code_of(&first), 0, "{}", stderr_of(&first));

    let second = scratch.run(Some(&relay.url), &["fetch", &token.expose(), "--copy"]);
    assert_eq!(code_of(&second), 27, "{}", stderr_of(&second));
    assert!(second.stdout.is_empty(), "{:?}", stdout_of(&second));
    let stderr = stderr_of(&second);
    assert!(stderr.contains("clift copy"), "{stderr}");
    assert!(
        !stderr.contains("clift paste"),
        "the outward direction's advice was given on a return trip: {stderr}"
    );

    // Without `--copy` it is the outward direction, and the outward advice is
    // the right one.
    let outward = relay.publish("shot.png", "image/png", b"once");
    let spent = scratch.run(Some(&relay.url), &["fetch", &outward.expose()]);
    assert_eq!(code_of(&spent), 0, "{}", stderr_of(&spent));
    let again = scratch.run(Some(&relay.url), &["fetch", &outward.expose()]);
    assert!(
        stderr_of(&again).contains("clift paste --copy"),
        "{}",
        stderr_of(&again)
    );
}

// --- The sending half, which needs a real clipboard ------------------------
//
// Behind `CLIFT_REAL_CLIPBOARD` for the same reason the rest of the clipboard
// tests are: running them replaces whatever the developer had copied. They are
// skipped loudly rather than quietly, because a test that silently does nothing
// is worse than one that is not there.

fn real_clipboard_or_skip(name: &str) -> bool {
    if std::env::var_os("CLIFT_REAL_CLIPBOARD").is_some() {
        return true;
    }
    announce(&format!(
        "SKIPPED {name}: CLIFT_REAL_CLIPBOARD is not set (it overwrites the real clipboard); \
         this test proved nothing"
    ));
    false
}

#[cfg(target_os = "macos")]
fn put_screenshot() {
    let status = Command::new("/usr/sbin/screencapture")
        .args(["-x", "-c"])
        .status()
        .expect("screencapture is macOS");
    assert!(status.success());
}

/// A markdown file, which is what showed that offering one representation was
/// not enough: it landed, the clipboard was deliberately left alone, and a
/// person who pressed a key saw nothing happen at all.
///
/// The assertion is against the clipboard itself, read back through the
/// product's own reader, not against what Clift said it did.
#[cfg(target_os = "macos")]
#[test]
fn a_text_attachment_puts_its_own_text_on_the_clipboard() {
    if !real_clipboard_or_skip("a_text_attachment_puts_its_own_text_on_the_clipboard") {
        return;
    }
    let relay = relay_or_skip!("a_text_attachment_puts_its_own_text_on_the_clipboard");
    let scratch = Scratch::new("copy-text");
    let body = "# heading\n\nA paragraph of markdown.\n";
    let token = relay.publish("notes.md", "text/markdown", body.as_bytes());

    let output = scratch.run(Some(&relay.url), &["fetch", &token.expose(), "--copy"]);
    assert_eq!(code_of(&output), 0, "{}", stderr_of(&output));
    assert!(output.stdout.is_empty(), "{:?}", stdout_of(&output));

    let clipboard = clift_clipboard::SystemClipboard::new();
    let snapshot = clipboard.read_snapshot().expect("readable");
    assert_eq!(
        snapshot.text.as_deref(),
        Some(body),
        "the markdown itself must be what a cursor receives"
    );
    // And the file is there too, so a paste into a folder produces it.
    let stderr = stderr_of(&output);
    let path = stderr
        .lines()
        .map(str::trim)
        .find(|line| line.ends_with("/notes.md"))
        .unwrap_or_else(|| panic!("no path was reported: {stderr}"));
    assert_eq!(fs::read_to_string(path).expect("the file must exist"), body);
}

/// An attachment that is neither a picture this build writes nor text: its path
/// goes on the clipboard, so a key press is never silent.
#[cfg(target_os = "macos")]
#[test]
fn an_attachment_with_no_direct_form_still_offers_its_path() {
    if !real_clipboard_or_skip("an_attachment_with_no_direct_form_still_offers_its_path") {
        return;
    }
    let relay = relay_or_skip!("an_attachment_with_no_direct_form_still_offers_its_path");
    let scratch = Scratch::new("copy-pdf");
    // Declared a PNG and not one. The signature check refuses to announce it as
    // a picture; the attachment is still delivered.
    let token = relay.publish("shot.png", "image/png", b"<html>not a picture</html>");

    let output = scratch.run(Some(&relay.url), &["fetch", &token.expose(), "--copy"]);
    assert_eq!(code_of(&output), 0, "{}", stderr_of(&output));

    let clipboard = clift_clipboard::SystemClipboard::new();
    let snapshot = clipboard.read_snapshot().expect("readable");
    let text = snapshot.text.unwrap_or_default();
    assert!(text.ends_with("/shot.png"), "expected a path, got {text:?}");
    assert!(
        snapshot.images.is_empty(),
        "bytes that only claim to be a PNG must not be announced as an image"
    );
}

/// Several attachments: one paste should produce all of them, so what goes on
/// the clipboard is the directory rather than an arbitrary one of the files.
#[cfg(target_os = "macos")]
#[test]
fn a_batch_of_several_offers_the_folder_that_holds_them() {
    if !real_clipboard_or_skip("a_batch_of_several_offers_the_folder_that_holds_them") {
        return;
    }
    let relay = relay_or_skip!("a_batch_of_several_offers_the_folder_that_holds_them");
    let scratch = Scratch::new("copy-many");
    let token = relay.publish_many(&[
        ("one.png", "image/png", b"first"),
        ("two.png", "image/png", b"second"),
    ]);

    let output = scratch.run(Some(&relay.url), &["fetch", &token.expose(), "--copy"]);
    assert_eq!(code_of(&output), 0, "{}", stderr_of(&output));
    let stderr = stderr_of(&output);
    assert!(stderr.contains("/one.png"), "{stderr}");
    assert!(stderr.contains("/two.png"), "{stderr}");

    let clipboard = clift_clipboard::SystemClipboard::new();
    let snapshot = clipboard.read_snapshot().expect("readable");
    let text = snapshot.text.unwrap_or_default();
    assert!(
        !text.ends_with("/one.png") && !text.ends_with("/two.png"),
        "one of the files was picked arbitrarily: {text:?}"
    );
    assert!(
        std::path::Path::new(&text).is_dir(),
        "expected the batch directory, got {text:?}"
    );
    // And the file reference is that same directory, so one paste in a folder
    // produces both attachments rather than one of them.
    assert_eq!(
        snapshot.files,
        vec![std::path::PathBuf::from(&text)],
        "the folder itself must be what a file manager receives"
    );
}

/// The return trip on a real clipboard: a file on one machine ends up as an
/// image on this one's clipboard, byte for byte.
///
/// The screenshot is only a convenient source of a genuine PNG -- what is being
/// tested is `clift copy` on one side and `clift fetch --copy` on the other,
/// with two separate homes between them. The comparison is against the bytes
/// that were published, not against whatever was on the clipboard before, so a
/// clipboard that quietly kept its old contents fails this rather than passing.
#[cfg(target_os = "macos")]
#[test]
fn a_file_copied_on_a_server_arrives_on_this_machines_clipboard() {
    if !real_clipboard_or_skip("a_file_copied_on_a_server_arrives_on_this_machines_clipboard") {
        return;
    }
    let relay = relay_or_skip!("a_file_copied_on_a_server_arrives_on_this_machines_clipboard");
    let sender = Scratch::new("return-send");
    let receiver = Scratch::new("return-receive");

    // A real PNG, produced by the system rather than invented here.
    let source = sender.0.join("screen.png");
    let status = Command::new("/usr/sbin/screencapture")
        .args(["-x", source.to_str().expect("a utf-8 path")])
        .status()
        .expect("screencapture is macOS");
    assert!(status.success());
    let published_bytes = fs::read(&source).expect("the screenshot must exist");

    // Something that is definitely not the picture, so that a clipboard which
    // was never written cannot pass by holding the right thing already.
    Command::new("/usr/bin/pbcopy")
        .stdin(Stdio::piped())
        .spawn()
        .and_then(|mut child| {
            child
                .stdin
                .as_mut()
                .expect("stdin")
                .write_all(b"placeholder")?;
            child.wait()
        })
        .expect("pbcopy is macOS");

    let published = sender.run(
        Some(&relay.url),
        &["copy", source.to_str().expect("a utf-8 path")],
    );
    assert_eq!(code_of(&published), 0, "{}", stderr_of(&published));
    let token = stdout_of(&published).trim().to_string();

    let redeemed = receiver.run(Some(&relay.url), &["fetch", &token, "--copy"]);
    assert_eq!(code_of(&redeemed), 0, "{}", stderr_of(&redeemed));
    assert!(
        redeemed.stdout.is_empty(),
        "--copy printed on stdout: {:?}",
        stdout_of(&redeemed)
    );
    assert!(
        stderr_of(&redeemed).contains("The clipboard now holds"),
        "{}",
        stderr_of(&redeemed)
    );

    // Read the PNG back with a tool that is not Clift. Its own reader cannot be
    // used here: the clipboard now also carries a file reference, and the source
    // priority makes a file list win over an image, so the reader would report
    // no image while the image is sitting right there. A ruler that shares a
    // rule with the thing it measures is not a ruler.
    let on_clipboard = clipboard_png().expect("a PNG must be on the clipboard");
    assert_eq!(
        on_clipboard, published_bytes,
        "the clipboard holds different bytes from the ones that were published"
    );

    // And the file reference alongside it, which is what a paste into a folder
    // uses.
    let clipboard = clift_clipboard::SystemClipboard::new();
    let snapshot = clipboard.read_snapshot().expect("readable");
    assert!(
        snapshot
            .files
            .iter()
            .any(|path| path.ends_with("screen.png")),
        "no file reference on the clipboard: {snapshot:?}"
    );
}

/// The clipboard's PNG representation, via AppleScript rather than via Clift.
#[cfg(target_os = "macos")]
fn clipboard_png() -> Option<Vec<u8>> {
    let output = Command::new("/usr/bin/osascript")
        .args(["-e", "the clipboard as «class PNGf»"])
        .output()
        .ok()?;
    let rendered = String::from_utf8_lossy(&output.stdout);
    let hex = rendered
        .split_once("«data PNGf")?
        .1
        .split_once('»')?
        .0
        .trim();
    (hex.len() % 2 == 0)
        .then(|| {
            (0..hex.len())
                .step_by(2)
                .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).ok())
                .collect::<Option<Vec<u8>>>()
        })
        .flatten()
}

/// The full loop, both halves, no target anywhere: a real screenshot on the
/// real clipboard goes to a real relay through the real binary, and a second
/// invocation of the real binary redeems the token and writes the file.
///
/// This is the closest an automated test can get to the product's claim. What
/// it cannot do is press a key in a terminal, which is why a manual
/// verification still exists.
#[cfg(target_os = "macos")]
#[test]
fn a_real_screenshot_makes_the_whole_round_trip_without_a_target() {
    if !real_clipboard_or_skip("a_real_screenshot_makes_the_whole_round_trip_without_a_target") {
        return;
    }
    let relay = relay_or_skip!("a_real_screenshot_makes_the_whole_round_trip_without_a_target");
    let scratch = Scratch::new("roundtrip");
    put_screenshot();

    let sent = scratch.run(
        Some(&relay.url),
        &["--json", "paste", "--mode", "universal"],
    );
    assert_eq!(code_of(&sent), 0, "{}", stderr_of(&sent));

    let document: serde_json::Value =
        serde_json::from_str(&stdout_of(&sent)).expect("stdout must be exactly one document");
    assert_eq!(document["mode"], "universal");
    // No host anywhere in the sending document: that is the whole point.
    assert!(document.get("target").is_none(), "{document}");
    let token = document["token"].as_str().expect("a token").to_string();
    assert!(token.starts_with("clift://v1/"), "{token}");
    assert!(
        document["insertion_text"]
            .as_str()
            .unwrap_or_default()
            .contains("clift fetch"),
        "{document}"
    );

    // And the far side, which in this test is the same machine but a separate
    // process that was told nothing except the token.
    let fetched = scratch.run(Some(&relay.url), &["fetch", &token, "--print-path"]);
    assert_eq!(code_of(&fetched), 0, "{}", stderr_of(&fetched));
    let path = stdout_of(&fetched).trim().to_string();
    assert!(path.ends_with(".png"), "{path}");
    let written = fs::metadata(&path).expect("the screenshot must be on disk");
    assert!(written.len() > 1000, "a screenshot should not be tiny");

    // Single use, over the whole real path.
    let again = scratch.run(Some(&relay.url), &["fetch", &token, "--print-path"]);
    assert_eq!(code_of(&again), 27, "{}", stderr_of(&again));
}

/// `--copy` puts the instruction on the clipboard and says so, and the text it
/// leaves there is the one an agent can run.
#[cfg(target_os = "macos")]
#[test]
fn copy_leaves_a_runnable_instruction_on_the_clipboard() {
    if !real_clipboard_or_skip("copy_leaves_a_runnable_instruction_on_the_clipboard") {
        return;
    }
    let relay = relay_or_skip!("copy_leaves_a_runnable_instruction_on_the_clipboard");
    let scratch = Scratch::new("copy");
    put_screenshot();

    let output = scratch.run(
        Some(&relay.url),
        &["paste", "--mode", "universal", "--copy"],
    );
    assert_eq!(code_of(&output), 0, "{}", stderr_of(&output));
    // The result went to the clipboard, so nothing may have gone to stdout.
    assert!(output.stdout.is_empty(), "{:?}", stdout_of(&output));

    let clipboard = Command::new("pbpaste").output().expect("pbpaste is macOS");
    let text = String::from_utf8_lossy(&clipboard.stdout).into_owned();
    assert!(text.contains("clift fetch '"), "{text}");
    assert!(
        !text.contains("--print-path"),
        "the path is printed by default: {text}"
    );
    assert!(!text.ends_with('\n'), "a trailing newline would submit it");
}

/// Plain text still costs nothing, in either mode. This is the plain-text rule restated for
/// Universal Mode: configuring a relay must not make an ordinary paste do
/// anything at all.
#[cfg(target_os = "macos")]
#[test]
fn plain_text_never_reaches_the_relay() {
    if !real_clipboard_or_skip("plain_text_never_reaches_the_relay") {
        return;
    }
    let relay = relay_or_skip!("plain_text_never_reaches_the_relay");
    let scratch = Scratch::new("plaintext");

    let mut child = Command::new("pbcopy")
        .stdin(Stdio::piped())
        .spawn()
        .expect("pbcopy is macOS");
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(b"just some words")
        .expect("write");
    assert!(child.wait().expect("wait").success());

    let output = scratch.run(Some(&relay.url), &["paste", "--mode", "universal"]);
    assert_eq!(code_of(&output), 10, "{}", stderr_of(&output));
    assert!(output.stdout.is_empty());

    // Nothing was published: the relay is still holding nothing.
    let settings = RelaySettings::new(&relay.url, 1024, MINUTE).expect("settings");
    assert_eq!(
        HttpRelay::new(&settings).health().expect("health").objects,
        0,
        "an ordinary paste reached the relay"
    );
}

/// Two configured hosts, one of them the default, and a Universal Mode paste
/// that must involve neither.
///
/// The proof is structural rather than argumentative: `ssh` and `sftp` are
/// replaced on `PATH` with scripts that record having been run. Nothing about
/// the assertion depends on reading Clift's own output, so it cannot be
/// satisfied by a message that merely claims no host was contacted.
///
/// This is the automated half of the specification's second red line under the new mode.
/// The other half -- that the *right* host receives it -- is a fact about where
/// the user pasted the token, and only a person can check that.
#[cfg(target_os = "macos")]
#[test]
fn universal_mode_contacts_neither_configured_host() {
    if !real_clipboard_or_skip("universal_mode_contacts_neither_configured_host") {
        return;
    }
    let relay = relay_or_skip!("universal_mode_contacts_neither_configured_host");
    let scratch = Scratch::new("twohosts");

    let config_dir = scratch.0.join("config").join("clift");
    fs::create_dir_all(&config_dir).unwrap();
    fs::write(
        config_dir.join("config.toml"),
        "version = 1\n\
         default_target = \"core\"\n\
         [targets.core]\nssh_host = \"core\"\n\
         [targets.hk]\nssh_host = \"hk\"\n",
    )
    .unwrap();

    // Stand-ins for the real clients, on a PATH of their own.
    let bin = scratch.0.join("bin");
    fs::create_dir_all(&bin).unwrap();
    let marker = scratch.0.join("contacted.log");
    for name in ["ssh", "sftp", "scp"] {
        let script = bin.join(name);
        fs::write(
            &script,
            format!(
                "#!/bin/sh\necho \"{name} $*\" >> {}\nexit 0\n",
                marker.display()
            ),
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
        }
    }

    put_screenshot();
    let output = Command::new(env!("CARGO_BIN_EXE_clift"))
        .args(["--json", "paste", "--mode", "universal"])
        .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
        .env("XDG_CONFIG_HOME", scratch.0.join("config"))
        .env("XDG_CACHE_HOME", scratch.0.join("cache"))
        .env("HOME", &scratch.0)
        .env("NO_COLOR", "1")
        .env("CLIFT_RELAY_URL", &relay.url)
        .output()
        .expect("the clift binary must be runnable");

    assert_eq!(code_of(&output), 0, "{}", stderr_of(&output));
    assert!(
        !marker.exists(),
        "Universal Mode ran an SSH client: {}",
        fs::read_to_string(&marker).unwrap_or_default()
    );

    // And it really did publish, so the absence above is not the absence of
    // any work at all.
    let document: serde_json::Value =
        serde_json::from_str(&stdout_of(&output)).expect("one document");
    assert!(
        document["token"]
            .as_str()
            .unwrap_or_default()
            .starts_with("clift://v1/")
    );
    let settings = RelaySettings::new(&relay.url, 1024, MINUTE).expect("settings");
    assert_eq!(
        HttpRelay::new(&settings).health().expect("health").objects,
        1
    );
}

/// `clift status` must report what would actually happen, not what the file
/// alone says.
///
/// The regression: `CLIFT_RELAY_URL` configures a relay, and `status` read only
/// the configuration file: so a user who set the variable was told "Mode:
/// fast, Relay: none" and then watched `clift paste` use the relay anyway. A
/// status command that disagrees with the command it is describing is worse
/// than no status command.
#[test]
fn status_reports_the_relay_the_environment_supplies() {
    let scratch = Scratch::new("statusenv");

    let without = scratch.run(None, &["status"]);
    assert!(
        stderr_of(&without).contains("Mode:    fast"),
        "{}",
        stderr_of(&without)
    );
    assert!(
        stderr_of(&without).contains("Relay:   none"),
        "{}",
        stderr_of(&without)
    );

    let with = scratch.run(Some("http://127.0.0.1:18999"), &["status"]);
    let text = stderr_of(&with);
    assert!(text.contains("Mode:    universal"), "{text}");
    assert!(text.contains("http://127.0.0.1:18999"), "{text}");

    // And the machine-readable document says the same thing.
    let json = scratch.run(Some("http://127.0.0.1:18999"), &["--json", "status"]);
    let document: serde_json::Value =
        serde_json::from_str(&stdout_of(&json)).expect("one document");
    assert_eq!(document["mode"], "universal");
    assert_eq!(document["relay"]["url"], "http://127.0.0.1:18999");
}

/// The same resolution, one layer down: a paste with only the environment set
/// must take the Universal path rather than failing over a missing target.
///
/// This one needs an attachment on the clipboard, and the requirement is not
/// incidental. `paste` reads the clipboard before it resolves a mode, so with
/// text on it *both* modes exit 10 for the same reason and the assertion below
/// cannot tell them apart. Arranging the attachment is therefore part of the
/// test rather than something to hope for -- it passed for a while only
/// because the developer's clipboard happened to hold a screenshot.
#[cfg(target_os = "macos")]
#[test]
fn a_relay_from_the_environment_alone_selects_universal_mode() {
    if !real_clipboard_or_skip("a_relay_from_the_environment_alone_selects_universal_mode") {
        return;
    }
    put_screenshot();
    let scratch = Scratch::new("envmode");
    // A configured default target, so that falling through to Fast Mode would
    // produce a *different* failure than the one asserted here.
    fs::create_dir_all(scratch.0.join("config").join("clift")).unwrap();
    fs::write(
        scratch.0.join("config").join("clift").join("config.toml"),
        "version = 1\ndefault_target = \"core\"\n[targets.core]\nssh_host = \"core\"\n",
    )
    .unwrap();

    // Nothing is listening on this port, so Universal Mode fails at the relay.
    // Reaching that failure at all is the assertion: Fast Mode would have gone
    // to `core` instead.
    let output = scratch.run(Some("http://127.0.0.1:1"), &["paste", "--to", "core"]);
    let text = stderr_of(&output);
    assert_eq!(code_of(&output), 20, "{text}");
    assert!(text.contains("--to"), "{text}");
    assert!(
        text.contains("Universal Mode does not send to one"),
        "{text}"
    );
}
