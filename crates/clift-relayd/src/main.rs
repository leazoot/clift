//! Clift's relay: a place to leave a few megabytes of ciphertext for a few
//! minutes.
//!
//! ```console
//! $ clift-relayd
//! clift-relayd listening on 127.0.0.1:8787
//! ```
//!
//! It is deliberately small enough to read in one sitting, because anybody who
//! is going to trust one has to be able to. What it can do is store bytes,
//! return bytes once, forget bytes, and say how many bytes it is holding. What
//! it cannot do is read them: the key never leaves the two machines at the ends
//! of the exchange, and there is no field in any request that could carry it.
//!
//! There is **no TLS here on purpose**. Terminating TLS is a job with a
//! well-solved answer -- a reverse proxy with a certificate the operator
//! already renews -- and a bespoke TLS listener in a 300-line service is a
//! liability rather than a feature. Run it behind nginx, Caddy or a tunnel; the
//! README says how. Over plain HTTP an eavesdropper sees ciphertext and object
//! ids, which is exactly what the relay itself sees, and still not the key.
//!
//! ## Configuration
//!
//! | Variable | Default | Meaning |
//! | --- | --- | --- |
//! | `CLIFT_RELAY_ADDR` | `127.0.0.1:8787` | Where to listen |
//! | `CLIFT_RELAY_MAX_BYTES` | `8MiB` | Largest single object |
//! | `CLIFT_RELAY_TTL` | `5m` | Longest an object may live |
//! | `CLIFT_RELAY_MAX_TOTAL_BYTES` | `256MiB` | Most the relay will hold at once |
//! | `CLIFT_RELAY_RATE_LIMIT` | `60` | Requests per minute per source; `0` disables |
//! | `CLIFT_RELAY_WORKERS` | `8` | Connection handling threads |

#![forbid(unsafe_code)]

mod ratelimit;
mod server;
mod store;

use clift_core::config::units::{parse_duration, parse_size};
use clift_core::universal::{DEFAULT_MAX_OBJECT_BYTES, DEFAULT_TTL, MAX_TTL};
use server::Relay;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;
use store::Store;

const DEFAULT_ADDRESS: &str = "127.0.0.1:8787";
const DEFAULT_MAX_TOTAL_BYTES: u64 = 256 * 1024 * 1024;
const DEFAULT_RATE_LIMIT: u32 = 60;
const DEFAULT_WORKERS: usize = 8;

/// How often expired objects are swept even when nothing is happening.
///
/// Writes and reads already drop what has expired, so this only matters for an
/// idle relay -- and an idle relay holding somebody's ciphertext until the next
/// request is exactly the case worth handling.
const SWEEP_INTERVAL: Duration = Duration::from_secs(30);

struct Settings {
    address: String,
    max_object_bytes: u64,
    max_total_bytes: u64,
    max_ttl: Duration,
    rate_limit: u32,
    workers: usize,
}

fn main() -> ExitCode {
    if std::env::args().any(|argument| argument == "--help" || argument == "-h") {
        eprintln!("{}", usage());
        return ExitCode::SUCCESS;
    }

    let settings = match settings_from_environment() {
        Ok(settings) => settings,
        Err(message) => {
            eprintln!("clift-relayd: {message}");
            return ExitCode::from(20);
        }
    };

    let store = Arc::new(Store::new(
        settings.max_object_bytes,
        settings.max_total_bytes,
    ));
    let relay = Arc::new(Relay::new(
        Arc::clone(&store),
        settings.rate_limit,
        settings.max_ttl,
    ));

    let http = match tiny_http::Server::http(&settings.address) {
        Ok(server) => Arc::new(server),
        Err(error) => {
            eprintln!(
                "clift-relayd: cannot listen on {}: {error}",
                settings.address
            );
            return ExitCode::from(20);
        }
    };

    // The address the socket actually got, not the one that was asked for.
    // They differ whenever the port was `0`, which is how a test starts a relay
    // without picking a port and hoping nothing else has it.
    let bound = match http.server_addr() {
        tiny_http::ListenAddr::IP(address) => address.to_string(),
        #[cfg(unix)]
        other => format!("{other:?}"),
    };
    eprintln!(
        "clift-relayd listening on {bound} (max object {} bytes, max ttl {}s, holding at most {} bytes)",
        settings.max_object_bytes,
        settings.max_ttl.as_secs(),
        settings.max_total_bytes,
    );

    let sweeper = Arc::clone(&store);
    if let Err(error) = std::thread::Builder::new()
        .name("clift-relay-sweep".to_string())
        .spawn(move || {
            loop {
                std::thread::sleep(SWEEP_INTERVAL);
                sweeper.sweep();
            }
        })
    {
        // Not fatal: every read and write sweeps too, so an idle relay simply
        // holds expired objects a little longer than it would have.
        eprintln!("clift-relayd: the expiry sweeper could not start: {error}");
    }

    let mut workers = Vec::with_capacity(settings.workers);
    for index in 0..settings.workers {
        let http = Arc::clone(&http);
        let relay = Arc::clone(&relay);
        match std::thread::Builder::new()
            .name(format!("clift-relay-{index}"))
            .spawn(move || {
                while let Ok(request) = http.recv() {
                    relay.handle(request);
                }
            }) {
            Ok(handle) => workers.push(handle),
            Err(error) => {
                eprintln!("clift-relayd: worker {index} could not start: {error}");
            }
        }
    }
    if workers.is_empty() {
        eprintln!("clift-relayd: no worker thread could be started");
        return ExitCode::from(30);
    }
    for worker in workers {
        let _ = worker.join();
    }
    ExitCode::SUCCESS
}

fn settings_from_environment() -> Result<Settings, String> {
    let max_object_bytes = size("CLIFT_RELAY_MAX_BYTES", DEFAULT_MAX_OBJECT_BYTES)?;
    let max_total_bytes = size("CLIFT_RELAY_MAX_TOTAL_BYTES", DEFAULT_MAX_TOTAL_BYTES)?;
    if max_object_bytes > max_total_bytes {
        return Err(format!(
            "CLIFT_RELAY_MAX_BYTES ({max_object_bytes}) is larger than CLIFT_RELAY_MAX_TOTAL_BYTES ({max_total_bytes}), so no object could ever be stored"
        ));
    }

    let max_ttl = match std::env::var("CLIFT_RELAY_TTL") {
        Ok(value) => parse_duration(&value)
            .map_err(|error| format!("CLIFT_RELAY_TTL: {}", error.reason()))?,
        Err(_) => DEFAULT_TTL,
    };
    if max_ttl.is_zero() || max_ttl > MAX_TTL {
        return Err(format!(
            "CLIFT_RELAY_TTL must be between 1 second and {} seconds",
            MAX_TTL.as_secs()
        ));
    }

    Ok(Settings {
        address: std::env::var("CLIFT_RELAY_ADDR").unwrap_or_else(|_| DEFAULT_ADDRESS.to_string()),
        max_object_bytes,
        max_total_bytes,
        max_ttl,
        rate_limit: number("CLIFT_RELAY_RATE_LIMIT", DEFAULT_RATE_LIMIT)?,
        workers: number::<usize>("CLIFT_RELAY_WORKERS", DEFAULT_WORKERS)?.clamp(1, 256),
    })
}

fn size(variable: &str, fallback: u64) -> Result<u64, String> {
    match std::env::var(variable) {
        Ok(value) => {
            let bytes =
                parse_size(&value).map_err(|error| format!("{variable}: {}", error.reason()))?;
            if bytes == 0 {
                return Err(format!("{variable} must be greater than zero"));
            }
            Ok(bytes)
        }
        Err(_) => Ok(fallback),
    }
}

fn number<T>(variable: &str, fallback: T) -> Result<T, String>
where
    T: std::str::FromStr,
    T::Err: std::fmt::Display,
{
    match std::env::var(variable) {
        Ok(value) => value
            .trim()
            .parse::<T>()
            .map_err(|error| format!("{variable}: {value:?} is not a whole number ({error})")),
        Err(_) => Ok(fallback),
    }
}

fn usage() -> String {
    "clift-relayd -- the relay for Clift's Universal Mode.\n\
     \n\
     It holds encrypted objects briefly and cannot read them. Configure it with\n\
     environment variables; there are no command line options.\n\
     \n\
       CLIFT_RELAY_ADDR             where to listen        (default 127.0.0.1:8787)\n\
       CLIFT_RELAY_MAX_BYTES        largest single object  (default 8MiB)\n\
       CLIFT_RELAY_TTL              longest object life    (default 5m, hard max 1h)\n\
       CLIFT_RELAY_MAX_TOTAL_BYTES  most held at once      (default 256MiB)\n\
       CLIFT_RELAY_RATE_LIMIT       requests/min/source    (default 60, 0 disables)\n\
       CLIFT_RELAY_WORKERS          handler threads        (default 8)\n\
     \n\
     Terminate TLS in front of it; it speaks plain HTTP by design."
        .to_string()
}
