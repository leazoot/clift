//! The relay client against a real relay, over a real socket -- twice.
//!
//! The specification forbids calling something end to end when the far side is a mock,
//! and this is the far side. Every scenario here runs against two real relays:
//! the `clift-relayd` binary on a real port, and the Cloudflare Worker in
//! `relay/cloudflare/` running in the real `workerd` runtime under
//! `wrangler dev`. The bytes go through a real TCP connection, a real HTTP
//! parse and a real `ureq` client either way. The only thing standing in for
//! anything is the payload, which is a few bytes rather than a screenshot.
//!
//! One set of scenarios, two backends, is the whole point: the Worker is a
//! second implementation of the same protocol, and the only thing that keeps
//! two implementations the same is one contract they both have to pass.
//!
//! Skipped loudly, never silently, when a backend is not available: the daemon
//! when it has not been built, the Worker when `npm install` has not been run
//! in `relay/cloudflare/`. `CLIFT_E2E_REQUIRE_WRANGLER` turns that skip into a
//! failure, the way `CLIFT_E2E_REQUIRE_DOCKER` does for the container tests.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use clift_core::ports::Relay;
use clift_core::universal::RelaySettings;
use clift_core::universal::token::{OBJECT_ID_BYTES, ObjectId};
use clift_relay::HttpRelay;
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};

fn announce(line: &str) {
    let mut stderr = std::io::stderr();
    let _ = writeln!(stderr, "{line}");
    let _ = stderr.flush();
}

/// `clift-relayd` lives beside the test executable, two directories up from
/// `target/<profile>/deps/`. `CARGO_BIN_EXE_` is only defined for binaries of
/// the same package, and a dev-dependency on the daemon would make one adapter
/// crate depend on another, which the architecture rules forbid.
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

/// A relay process that is killed when the test ends, however it ends.
struct Relayd {
    child: Child,
    url: String,
}

impl Relayd {
    /// Starts one on a port the operating system chooses, and waits for it to
    /// say which port that was.
    fn start(extra: &[(&str, &str)]) -> Option<Self> {
        let binary = relayd_binary()?;
        let mut command = Command::new(binary);
        command
            .env("CLIFT_RELAY_ADDR", "127.0.0.1:0")
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        for (name, value) in extra {
            command.env(name, value);
        }
        let mut child = command.spawn().ok()?;

        let stderr = child.stderr.take()?;
        let mut reader = BufReader::new(stderr);
        let mut line = String::new();
        // The first line is the banner, and it carries the bound address.
        // Reading it is also how the test knows the socket is accepting.
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

        // The remaining output is drained on a thread so a chatty relay cannot
        // fill its pipe and block itself.
        std::thread::spawn(move || {
            let mut sink = String::new();
            while reader.read_line(&mut sink).unwrap_or(0) > 0 {
                sink.clear();
            }
        });

        Some(Self { child, url })
    }

    fn settings(&self, ttl: Duration) -> RelaySettings {
        RelaySettings::new(&self.url, 8 * 1024 * 1024, ttl).expect("the test URL must be valid")
    }
}

impl Drop for Relayd {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// What a scenario needs from whichever relay it is talking to.
trait RelayUnderTest {
    fn settings(&self, ttl: Duration) -> RelaySettings;
    fn client(&self, ttl: Duration) -> HttpRelay {
        HttpRelay::new(&self.settings(ttl))
    }
}

impl RelayUnderTest for Relayd {
    fn settings(&self, ttl: Duration) -> RelaySettings {
        Relayd::settings(self, ttl)
    }
}

/// The Worker directory, two levels above this crate.
fn worker_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../relay/cloudflare")
        .canonicalize()
        .unwrap_or_else(|_| PathBuf::from("relay/cloudflare"))
}

/// `wrangler` as `npm install` put it, or the reason it is not there.
fn wrangler_binary() -> Result<PathBuf, String> {
    if cfg!(windows) {
        return Err("the wrangler-backed contract tests are not wired up on Windows".to_string());
    }
    let candidate = worker_dir().join("node_modules/.bin/wrangler");
    if candidate.is_file() {
        Ok(candidate)
    } else {
        Err(format!(
            "{} is missing; run `npm install` in relay/cloudflare",
            candidate.display()
        ))
    }
}

/// `wrangler dev` starts one `workerd` and one Node process each time, so the
/// Worker-backed scenarios take turns rather than starting nine at once.
static WORKER_TURN: Mutex<()> = Mutex::new(());

/// The Worker running in the real `workerd` runtime, on a port of the test's
/// choosing, with its storage in a directory that is thrown away afterwards.
struct WorkerDev {
    child: Child,
    url: String,
    state: PathBuf,
    _turn: MutexGuard<'static, ()>,
}

impl WorkerDev {
    fn start(extra: &[(&str, &str)]) -> Result<Self, String> {
        let wrangler = wrangler_binary()?;
        let turn = WORKER_TURN
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());

        // A port the operating system says is free right now. `wrangler dev`
        // has no "port 0" of its own.
        let port = TcpListener::bind("127.0.0.1:0")
            .and_then(|listener| listener.local_addr())
            .map(|address| address.port())
            .map_err(|error| format!("could not find a free port: {error}"))?;

        let state =
            std::env::temp_dir().join(format!("clift-worker-{}-{port}", std::process::id()));
        std::fs::create_dir_all(&state)
            .map_err(|error| format!("could not create {}: {error}", state.display()))?;

        let mut command = Command::new(wrangler);
        command
            .current_dir(worker_dir())
            .args(["dev", "--ip", "127.0.0.1", "--port", &port.to_string()])
            .arg("--persist-to")
            .arg(&state)
            .args(["--log-level", "error"])
            // The Worker does not talk to Cloudflare in local mode, and a
            // test must not start doing so because somebody logged in once.
            .env_remove("CLOUDFLARE_API_TOKEN")
            .env_remove("CLOUDFLARE_ACCOUNT_ID")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        for (name, value) in extra {
            command.arg("--var").arg(format!("{name}:{value}"));
        }
        let child = command
            .spawn()
            .map_err(|error| format!("could not start wrangler: {error}"))?;

        let mut started = Self {
            child,
            url: format!("http://127.0.0.1:{port}"),
            state,
            _turn: turn,
        };

        // Ready means the health endpoint answers, which is the same thing a
        // client would need to be true.
        let deadline = Instant::now() + Duration::from_secs(90);
        loop {
            if started.client(MINUTE).health().is_ok() {
                return Ok(started);
            }
            if let Ok(Some(status)) = started.child.try_wait() {
                return Err(format!("wrangler dev exited before it was ready: {status}"));
            }
            if Instant::now() > deadline {
                return Err("wrangler dev did not become ready within 90 seconds".to_string());
            }
            std::thread::sleep(Duration::from_millis(200));
        }
    }
}

impl RelayUnderTest for WorkerDev {
    fn settings(&self, ttl: Duration) -> RelaySettings {
        RelaySettings::new(&self.url, 8 * 1024 * 1024, ttl).expect("the test URL must be valid")
    }
}

impl Drop for WorkerDev {
    fn drop(&mut self) {
        // SIGTERM rather than SIGKILL: wrangler owns a workerd child, and only
        // a signal it gets to handle lets it take that child down with it.
        let _ = Command::new("kill")
            .args(["-TERM", &self.child.id().to_string()])
            .status();
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            if matches!(self.child.try_wait(), Ok(Some(_))) {
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.state);
    }
}

/// Starts the Worker, or says why the test proved nothing and stops.
fn worker_or_skip(name: &str, extra: &[(&str, &str)]) -> Option<WorkerDev> {
    match WorkerDev::start(extra) {
        Ok(worker) => Some(worker),
        Err(reason) => {
            assert!(
                std::env::var_os("CLIFT_E2E_REQUIRE_WRANGLER").is_none(),
                "CLIFT_E2E_REQUIRE_WRANGLER is set but the Worker could not start: {reason}"
            );
            announce(&format!(
                "SKIPPED {name} (cloudflare worker): {reason}; this test proved nothing"
            ));
            None
        }
    }
}

/// One scenario, two tests: `<scenario>::on_relayd` and
/// `<scenario>::on_cloudflare_worker`. Each starts its own relay with the
/// given environment and skips loudly if that backend is unavailable.
macro_rules! contract {
    ($scenario:ident $(, $env:expr)*) => {
        mod $scenario {
            use super::*;

            #[test]
            fn on_relayd() {
                let Some(relay) = Relayd::start(&[$($env),*]) else {
                    announce(concat!(
                        "SKIPPED ", stringify!($scenario),
                        " (relayd): clift-relayd is not built (cargo build -p clift-relayd); \
                         this test proved nothing"
                    ));
                    return;
                };
                super::$scenario(&relay);
            }

            #[test]
            fn on_cloudflare_worker() {
                let Some(relay) = worker_or_skip(stringify!($scenario), &[$($env),*]) else {
                    return;
                };
                super::$scenario(&relay);
            }
        }
    };
}

const MINUTE: Duration = Duration::from_secs(60);

fn an_object_goes_up_and_comes_back_down(relay: &dyn RelayUnderTest) {
    let client = relay.client(MINUTE);

    let published = client.publish(b"sealed bytes", MINUTE).expect("publish");
    assert_eq!(published.ttl, MINUTE);
    assert_eq!(
        client.retrieve(&published.id).expect("retrieve"),
        b"sealed bytes".to_vec()
    );
}
contract!(an_object_goes_up_and_comes_back_down);

/// The single-use rule, enforced by the relay rather than by the client. This
/// is the property the whole token design rests on.
fn the_second_fetch_of_an_object_fails_as_gone(relay: &dyn RelayUnderTest) {
    let client = relay.client(MINUTE);

    let published = client.publish(b"once only", MINUTE).expect("publish");
    assert!(client.retrieve(&published.id).is_ok());

    let error = client.retrieve(&published.id).unwrap_err();
    assert_eq!(
        error.kind(),
        clift_core::error::ErrorKind::TokenUnusable,
        "a spent object must read as spent, not as a broken relay"
    );
    assert_eq!(error.exit_code().as_u8(), 27);
}
contract!(the_second_fetch_of_an_object_fails_as_gone);

fn an_object_that_was_never_published_is_not_there(relay: &dyn RelayUnderTest) {
    let client = relay.client(MINUTE);

    let error = client
        .retrieve(&ObjectId::from_bytes([9; OBJECT_ID_BYTES]))
        .unwrap_err();
    assert_eq!(error.kind(), clift_core::error::ErrorKind::TokenUnusable);
}
contract!(an_object_that_was_never_published_is_not_there);

fn a_withdrawn_object_cannot_be_fetched_and_withdrawing_twice_is_fine(relay: &dyn RelayUnderTest) {
    let client = relay.client(MINUTE);

    let published = client.publish(b"regretted", MINUTE).expect("publish");
    client.revoke(&published.id).expect("revoke");
    assert!(client.retrieve(&published.id).is_err());
    // Idempotent: the caller wanted it gone, and it is.
    client.revoke(&published.id).expect("second revoke");
}
contract!(a_withdrawn_object_cannot_be_fetched_and_withdrawing_twice_is_fine);

/// The relay's ceiling, refused over the wire rather than by the client.
fn an_object_over_the_relays_limit_is_refused_with_the_size_error(relay: &dyn RelayUnderTest) {
    // The client's own ceiling is deliberately higher than the relay's, so the
    // refusal being tested is the relay's.
    let client = relay.client(MINUTE);

    let error = client.publish(&vec![0_u8; 4096], MINUTE).unwrap_err();
    assert_eq!(error.kind(), clift_core::error::ErrorKind::LimitExceeded);
    assert_eq!(error.exit_code().as_u8(), 26);

    // And something within the limit still works, so the relay was refusing the
    // size rather than being broken.
    assert!(client.publish(&vec![0_u8; 512], MINUTE).is_ok());
}
contract!(
    an_object_over_the_relays_limit_is_refused_with_the_size_error,
    ("CLIFT_RELAY_MAX_BYTES", "1KiB")
);

fn an_expired_object_is_gone_without_anybody_asking(relay: &dyn RelayUnderTest) {
    let client = relay.client(Duration::from_secs(1));

    let published = client
        .publish(b"short lived", Duration::from_secs(1))
        .expect("publish");
    std::thread::sleep(Duration::from_millis(1500));

    let error = client.retrieve(&published.id).unwrap_err();
    assert_eq!(error.kind(), clift_core::error::ErrorKind::TokenUnusable);
}
contract!(an_expired_object_is_gone_without_anybody_asking);

/// Eight threads racing for one object. Exactly one may get it: an object that
/// two hosts could both fetch is an object that reached the wrong host.
fn only_one_of_many_simultaneous_fetches_wins(relay: &dyn RelayUnderTest) {
    let client = relay.client(MINUTE);
    let published = client.publish(b"contested", MINUTE).expect("publish");

    let barrier = std::sync::Arc::new(std::sync::Barrier::new(8));
    let mut handles = Vec::new();
    for _ in 0..8 {
        let barrier = std::sync::Arc::clone(&barrier);
        let settings = relay.settings(MINUTE);
        let id = published.id.clone();
        handles.push(std::thread::spawn(move || {
            let client = HttpRelay::new(&settings);
            barrier.wait();
            client.retrieve(&id).is_ok()
        }));
    }
    let winners = handles
        .into_iter()
        .filter(|_| true)
        .filter_map(|handle| handle.join().ok())
        .filter(|won| *won)
        .count();
    assert_eq!(winners, 1, "the object was handed out {winners} times");
}
contract!(only_one_of_many_simultaneous_fetches_wins);

fn the_health_endpoint_reports_what_the_relay_is_holding(relay: &dyn RelayUnderTest) {
    let client = relay.client(MINUTE);

    let before = client.health().expect("health");
    assert_eq!(before.objects, 0);
    assert!(before.max_object_bytes > 0);
    assert!(before.max_ttl_seconds > 0);

    client.publish(b"held", MINUTE).expect("publish");
    assert_eq!(client.health().expect("health").objects, 1);
}
contract!(the_health_endpoint_reports_what_the_relay_is_holding);

/// Nothing the client sends may carry key material, and the only way to be sure
/// is to look at what actually goes over the socket. The relay records the URL
/// of every request; none of them may contain anything that is not an id.
fn no_request_the_client_makes_carries_anything_but_an_object_id(relay: &dyn RelayUnderTest) {
    let client = relay.client(MINUTE);

    let published = client.publish(b"sealed", MINUTE).expect("publish");
    let encoded = published.id.encoded();
    // The id is 22 base64url characters and nothing else -- no fragment, no
    // query, nothing that could be a key.
    assert_eq!(encoded.len(), 22);
    assert!(
        encoded
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_'),
        "{encoded}"
    );
    assert!(client.retrieve(&published.id).is_ok());
}
contract!(no_request_the_client_makes_carries_anything_but_an_object_id);

#[test]
fn a_relay_that_is_not_listening_is_reported_as_unavailable() {
    // No process at all: port 1 on loopback is not going to answer.
    let settings = RelaySettings::new("http://127.0.0.1:1", 1024, MINUTE).expect("settings");
    let client = HttpRelay::new(&settings);

    let error = client.publish(b"x", MINUTE).unwrap_err();
    assert_eq!(error.kind(), clift_core::error::ErrorKind::RelayUnavailable);
    assert_eq!(error.exit_code().as_u8(), 28);
    assert!(error.remedy().is_some());
}
